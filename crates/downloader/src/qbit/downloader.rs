use super::auth::trimmed_api_key;
use super::*;

impl Downloader for QbitDownloader {
    fn submit_verified(
        &self,
        payload: &crate::SubmitPayload,
        options: &crate::SubmitOptions,
    ) -> Result<crate::ClientSubmitResult, DownloaderError> {
        let info_hash = payload.info_hash().trim().to_ascii_lowercase();
        if info_hash.is_empty() || options.ownership_tag.trim().is_empty() {
            return Err(DownloaderError::Message(
                "info_hash and ownership_tag must not be empty".into(),
            ));
        }
        let tags_csv = build_qbit_tags(options)?;
        if let Some(existing) = self.check_existing_qbit_task(&info_hash, options, &tags_csv)? {
            return Ok(existing);
        }

        let temp_guard = match payload {
            crate::SubmitPayload::TorrentBytes { bytes, .. } => {
                Some(TempFileGuard::write_torrent(&options.ownership_tag, bytes)?)
            }
            crate::SubmitPayload::Magnet { .. } => None,
        };
        if let Some(cat) = options.category.as_deref()
            && let Err(reason) = crate::validate_category_name(cat)
        {
            return Err(DownloaderError::Message(reason.into()));
        }
        let category = options
            .category
            .as_deref()
            .or(self.config.category.as_deref())
            .filter(|c| !c.trim().is_empty());
        if let Some(cat) = category
            && let Err(reason) = crate::validate_category_name(cat)
        {
            return Err(DownloaderError::Message(reason.into()));
        }
        let up_limit_str = options.upload_limit.map(|v| v.to_string());
        let dl_limit_str = options.download_limit.map(|v| v.to_string());

        let build_form = || {
            let paused_str = if options.paused { "true" } else { "false" };
            let mut form = ureq::unversioned::multipart::Form::new()
                .text("paused", paused_str)
                .text("stopped", paused_str)
                .text("tags", tags_csv.as_str());
            if let Some(save_path) = options
                .save_path
                .as_deref()
                .filter(|p| !p.trim().is_empty())
            {
                form = form.text("savepath", save_path);
            }
            if let Some(cat) = category {
                form = form.text("category", cat);
            }
            if let Some(ul) = up_limit_str.as_deref() {
                form = form.text("upLimit", ul);
            }
            if let Some(dl) = dl_limit_str.as_deref() {
                form = form.text("dlLimit", dl);
            }
            if options.sequential {
                form = form.text("sequentialDownload", "true");
            }
            if options.first_last_piece {
                form = form.text("firstLastPiecePrio", "true");
            }
            match payload {
                crate::SubmitPayload::TorrentBytes { .. } => {
                    if let Some(ref guard) = temp_guard {
                        let part = build_torrent_file_part(&guard.path)?;
                        form = form.part("torrents", part.file_name("download.torrent"));
                    }
                }
                crate::SubmitPayload::Magnet { uri, .. } => {
                    form = form.text("urls", uri.as_str());
                }
            }
            Ok(form)
        };

        self.post_verified_add_form(&build_form, &info_hash, &tags_csv)
    }

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<crate::ClientTaskQuery, DownloaderError> {
        let Some(info) = self.find_info_by_hash(info_hash)? else {
            return Ok(crate::ClientTaskQuery::default());
        };
        let owned_by_tag = !ownership_tag.trim().is_empty()
            && info
                .tags
                .split(',')
                .map(str::trim)
                .any(|t| t == ownership_tag);
        let health = existing_task_health(&info.state, info.progress);
        let paused = health == ExistingHealth::Paused
            || info.state.starts_with("paused")
            || info.state.starts_with("stopped");
        Ok(crate::ClientTaskQuery {
            exists: true,
            info_hash: Some(info.hash.to_ascii_lowercase()),
            owned_by_tag,
            broken: health == ExistingHealth::Broken,
            state: Some(info.state),
            paused,
        })
    }

    fn uploaded_by_category(&self, category: &str) -> Result<u64, DownloaderError> {
        let url = format!(
            "{}?category={}",
            join(&self.config.url, "/api/v2/torrents/info"),
            form_enc(category)
        );
        let body = self.get(&url)?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(infos.iter().map(|info| info.uploaded).sum())
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.remove_owned(torrent, delete_files)
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        Ok(match self.exact_existing_hash(torrent)? {
            Some(hash) => self.hash_claimed(&hash),
            None => false,
        })
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        self.exact_existing_hash(torrent)
    }

    fn endpoint(&self) -> Option<String> {
        Some(format!(
            "qbittorrent|{}",
            self.config.url.trim_end_matches('/')
        ))
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        let Some(hash) = crate::owned::owned_removal_target(
            self.owned_identity(torrent),
            &torrent.enclosure,
            false,
        )?
        else {
            return Ok(());
        };
        let delete_files = delete_files && !self.hash_claimed(&hash);
        tracing::info!(torrent = %torrent.title, hash = %hash, delete_files = %delete_files, "从 qBittorrent 移除已证实归属的种子");
        let url = join(&self.config.url, "/api/v2/torrents/delete");
        let body = format!("hashes={}&deleteFiles={}", form_enc(&hash), delete_files);
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        let Some(info) = self.owned_info(torrent)? else {
            return Ok(Vec::new());
        };
        let listed = self.torrent_files(&info.hash)?;
        // F-1: skipped (priority 0) files never complete; the task is done once
        // every wanted file is.
        let wanted_done = listed.iter().any(|file| file.priority > 0)
            && listed
                .iter()
                .filter(|file| file.priority > 0)
                .all(|file| file.progress >= 1.0);
        if info.progress < 1.0 && !wanted_done {
            return Ok(Vec::new());
        }
        Ok(listed
            .into_iter()
            .filter(|file| file.priority > 0 && file.progress >= 1.0)
            .map(|file| Path::new(&info.save_path).join(file.name))
            .map(|path| apply_maps(&path, &self.config.path_maps))
            .collect())
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        let Some(info) = self.owned_info(torrent)? else {
            return Ok((Vec::new(), false));
        };
        let listed = self.torrent_files(&info.hash)?;
        let wanted_done = listed.iter().any(|file| file.priority > 0)
            && listed
                .iter()
                .filter(|file| file.priority > 0)
                .all(|file| file.progress >= 1.0);
        let done = info.progress >= 1.0 || wanted_done;
        let files = listed
            .into_iter()
            .filter(|file| file.priority > 0 && file.progress >= 1.0)
            .map(|file| Path::new(&info.save_path).join(file.name))
            .map(|path| apply_maps(&path, &self.config.path_maps))
            .collect();
        Ok((files, done))
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        let Some(hash) = self.exact_existing_hash(torrent)? else {
            return Ok(None);
        };
        let files = self.torrent_files(&hash)?;
        if files.is_empty() {
            return Ok(None); // metadata not received yet
        }
        if crate::selection_drops_all_videos(files.iter().map(|f| f.name.as_str()), keep) {
            tracing::warn!(hash = %hash, "选择性下载会跳过全部视频文件，放弃筛选、整包下载");
            return Ok(Some(0));
        }
        let skipped: Vec<String> = files
            .into_iter()
            .enumerate()
            .filter(|(_, file)| !keep(&file.name))
            .map(|(i, file)| file.index.unwrap_or(i).to_string())
            .collect();
        if skipped.is_empty() {
            return Ok(Some(0));
        }
        tracing::info!(hash = %hash, count = skipped.len(), "qBittorrent 跳过不需要的文件");
        let url = join(&self.config.url, "/api/v2/torrents/filePrio");
        let body = format!(
            "hash={}&id={}&priority=0",
            form_enc(&hash),
            form_enc(&skipped.join("|"))
        );
        let _ = self.post_form(&url, &body)?;
        Ok(Some(skipped.len()))
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        let delete_files = delete_files && !self.hash_claimed(info_hash);
        tracing::info!(hash = %info_hash, delete_files, "从 qBittorrent 删除任务");
        let url = join(&self.config.url, "/api/v2/torrents/delete");
        let body = format!(
            "hashes={}&deleteFiles={}",
            form_enc(info_hash),
            delete_files
        );
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "暂停 qBittorrent 任务");
        let url = join(&self.config.url, "/api/v2/torrents/pause");
        let body = format!("hashes={}", form_enc(info_hash));
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "恢复 qBittorrent 任务");
        let url = join(&self.config.url, "/api/v2/torrents/resume");
        let body = format!("hashes={}", form_enc(info_hash));
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        let prefs_url = join(&self.config.url, "/api/v2/app/preferences");
        let body = self.get(&prefs_url)?;
        let prefs: serde_json::Value =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        let dl = prefs["dl_limit"].as_u64().unwrap_or(0);
        let ul = prefs["up_limit"].as_u64().unwrap_or(0);
        Ok((dl, ul))
    }

    fn set_speed_limits(
        &self,
        download_limit: u64,
        upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        tracing::info!(
            dl_limit = download_limit,
            up_limit = upload_limit,
            "设置 qBittorrent 速度限制"
        );
        let url = join(&self.config.url, "/api/v2/app/setPreferences");
        let payload = format!(
            "json={}",
            form_enc(&format!(
                "{{\"dl_limit\":{},\"up_limit\":{}}}",
                download_limit, upload_limit
            ))
        );
        let response = self.authed(|auth_name, auth_val| {
            agent_for_url(&self.config.url, true)
                .post(&url)
                .header("Referer", &self.config.url)
                .header("Origin", self.config.url.trim_end_matches('/'))
                .header(auth_name, auth_val)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(&payload)
        })?;
        let _ = read_body(response)?;
        Ok(())
    }

    /// Enumerate every task. Our own deliveries carry the constant
    /// `crawler-media` tag and are matched by it; per-torrent identity is
    /// reported through `owned_tags`. Foreign torrents in the same client have
    /// no such tag and are reported too (the task center groups them as 刷流做种).
    fn task_snapshots(&self) -> Result<Vec<crate::TaskSnapshot>, DownloaderError> {
        let url = join(&self.config.url, "/api/v2/torrents/info");
        let body = self.get(&url)?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(infos
            .into_iter()
            .map(|info| {
                let tag = our_tag(&info.tags);
                let owned_tags = crate::owned_tags_from(info.tags.split(','));
                crate::TaskSnapshot {
                    owned_tags,
                    tag,
                    name: info.name,
                    progress: info.progress,
                    state: info.state,
                    size_bytes: info.size,
                    downloaded_bytes: info.downloaded,
                    uploaded_bytes: info.uploaded,
                    download_speed: info.dlspeed,
                    upload_speed: info.upspeed,
                    info_hash: info.hash,
                }
            })
            .collect())
    }

    fn task_files_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<crate::TaskFileSet>, DownloaderError> {
        self.qbit_task_files_by_hash(info_hash)
    }

    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        self.qbit_set_task_tags(info_hash, tags)
    }

    fn set_task_category(&self, info_hash: &str, category: &str) -> Result<(), DownloaderError> {
        self.qbit_set_task_category(info_hash, category)
    }

    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        self.qbit_set_task_limits(info_hash, upload_limit, download_limit)
    }

    fn disk_free_space(&self) -> Result<crate::DiskSpaceInfo, DownloaderError> {
        self.qbit_disk_free_space()
    }
}

impl QbitDownloader {
    /// Review H-3: the task carries `CLAIMED_TAG`. Unknown (lookup failed)
    /// counts as claimed so user data is never deleted on doubt.
    fn hash_claimed(&self, hash: &str) -> bool {
        let url = format!(
            "{}?hashes={}",
            join(&self.config.url, "/api/v2/torrents/info"),
            form_enc(hash)
        );
        let infos: Result<Vec<TorrentInfo>, _> = self.get(&url).and_then(|body| {
            serde_json::from_str(&body).map_err(|e| DownloaderError::Message(e.to_string()))
        });
        match infos {
            Ok(infos) => infos.iter().any(|info| {
                info.hash.eq_ignore_ascii_case(hash)
                    && info.tags.split(',').any(|t| t.trim() == crate::CLAIMED_TAG)
            }),
            Err(error) => {
                tracing::warn!(%hash, %error, "无法确认任务是否为认领的用户任务，按认领处理（不删文件）");
                true
            }
        }
    }

    pub(super) fn find_info_by_hash(
        &self,
        hash: &str,
    ) -> Result<Option<TorrentInfo>, DownloaderError> {
        let normalized = hash.trim().to_ascii_lowercase();
        let url = format!(
            "{}?hashes={}",
            join(&self.config.url, "/api/v2/torrents/info"),
            form_enc(&normalized)
        );
        let infos: Vec<TorrentInfo> = serde_json::from_str(&self.get(&url)?)
            .map_err(|e| DownloaderError::Message(e.to_string()))?;
        Ok(infos
            .into_iter()
            .find(|info| info.hash.eq_ignore_ascii_case(&normalized)))
    }

    /// D-L2: tasks that may belong to `torrent`, asked for with a `hashes=`
    /// (magnet) or `tag=` filter so the client does not list every task.
    /// Results are still checked here, so a client ignoring the filter
    /// stays correct.
    fn candidate_infos(&self, torrent: &Torrent) -> Result<Vec<TorrentInfo>, DownloaderError> {
        let magnet = crate::magnet_info_hash(&torrent.enclosure);
        let tag = crate::ownership_tag(&torrent.enclosure);
        let filter = match &magnet {
            Some(hash) => format!("hashes={}", form_enc(hash)),
            None => format!("tag={}", form_enc(&tag)),
        };
        let url = format!(
            "{}?{filter}",
            join(&self.config.url, "/api/v2/torrents/info")
        );
        let infos: Vec<TorrentInfo> = serde_json::from_str(&self.get(&url)?)
            .map_err(|e| DownloaderError::Message(e.to_string()))?;
        Ok(infos
            .into_iter()
            .filter(|info| {
                if let Some(hash) = &magnet {
                    info.hash.eq_ignore_ascii_case(hash)
                } else {
                    info.tags.split(',').any(|item| item.trim() == tag)
                }
            })
            .collect())
    }

    fn exact_existing_hash(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        let hashes = self
            .candidate_infos(torrent)?
            .into_iter()
            .map(|info| info.hash)
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    pub(super) fn torrent_files(&self, hash: &str) -> Result<Vec<TorrentFile>, DownloaderError> {
        let files_url = format!(
            "{}?hash={}",
            join(&self.config.url, "/api/v2/torrents/files"),
            form_enc(hash)
        );
        let file_body = self.get(&files_url)?;
        serde_json::from_str(&file_body).map_err(|err| DownloaderError::Message(err.to_string()))
    }

    fn owned_info(&self, torrent: &Torrent) -> Result<Option<TorrentInfo>, DownloaderError> {
        // D-L2: one filtered request instead of two full listings.
        let infos = self.candidate_infos(torrent)?;
        let hashes = infos.iter().map(|info| info.hash.clone()).collect();
        let Some(hash) = crate::owned::proven_hashes(hashes)? else {
            return Ok(None);
        };
        Ok(infos
            .into_iter()
            .find(|info| info.hash.eq_ignore_ascii_case(&hash)))
    }

    pub(super) fn get(&self, url: &str) -> Result<String, DownloaderError> {
        let response = self.authed(|auth_name, auth_val| {
            agent_for_url(&self.config.url, true)
                .get(url)
                .header("Referer", &self.config.url)
                .header("Origin", self.config.url.trim_end_matches('/'))
                .header(auth_name, auth_val)
                .call()
        })?;
        read_body(response)
    }

    pub(super) fn post_form(&self, url: &str, body: &str) -> Result<String, DownloaderError> {
        let response = self.authed(|auth_name, auth_val| {
            agent_for_url(&self.config.url, true)
                .post(url)
                .header("Referer", &self.config.url)
                .header("Origin", self.config.url.trim_end_matches('/'))
                .header(auth_name, auth_val)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(body)
        })?;
        read_body(response)
    }

    fn post_verified_add_form<'f>(
        &self,
        form: &dyn Fn() -> Result<ureq::unversioned::multipart::Form<'f>, DownloaderError>,
        info_hash: &str,
        tags: &str,
    ) -> Result<crate::ClientSubmitResult, DownloaderError> {
        let url = join(&self.config.url, "/api/v2/torrents/add");
        let send_once = |auth_name: &str,
                         auth_val: &str|
         -> Result<ureq::http::Response<ureq::Body>, SendOrBuildErr> {
            let built = form().map_err(SendOrBuildErr::Build)?;
            agent_for_url(&self.config.url, true)
                .post(&url)
                .header("Referer", &self.config.url)
                .header("Origin", self.config.url.trim_end_matches('/'))
                .header(auth_name, auth_val)
                .send(built)
                .map_err(SendOrBuildErr::Http)
        };
        let response = if let Some(key) = trimmed_api_key(&self.config) {
            let bearer = format!("Bearer {key}");
            match send_once("Authorization", &bearer) {
                Err(SendOrBuildErr::Build(err)) => return Err(err),
                Err(SendOrBuildErr::Http(ureq::Error::StatusCode(code @ (401 | 403)))) => {
                    tracing::error!(url = %self.config.url, status = code, "qBittorrent API Key 认证失败");
                    return Err(DownloaderError::Message(format!(
                        "qBittorrent API Key authentication failed ({code})"
                    )));
                }
                Err(SendOrBuildErr::Http(e)) => return classify_qbit_send_err(e),
                Ok(resp) => resp,
            }
        } else {
            let cookie = self.cookie();
            match send_once("Cookie", &cookie) {
                Err(SendOrBuildErr::Build(err)) => return Err(err),
                Err(SendOrBuildErr::Http(ureq::Error::StatusCode(403))) => {
                    self.relogin(&cookie)?;
                    match send_once("Cookie", &self.cookie()) {
                        Err(SendOrBuildErr::Build(err)) => return Err(err),
                        Err(SendOrBuildErr::Http(ureq::Error::StatusCode(403))) => {
                            return Err(DownloaderError::Message(
                                "qBittorrent authentication failed (403 after re-login)".into(),
                            ));
                        }
                        Err(SendOrBuildErr::Http(e)) => return classify_qbit_send_err(e),
                        Ok(resp) => resp,
                    }
                }
                Err(SendOrBuildErr::Http(e)) => return classify_qbit_send_err(e),
                Ok(resp) => resp,
            }
        };

        let body = match read_body(response) {
            Ok(body) => body,
            Err(err) => {
                tracing::warn!(info_hash = %info_hash, error = %err, "qBittorrent /torrents/add 响应体读取失败，标记 Unknown");
                return Ok(crate::ClientSubmitResult::UnknownOutcome {
                    reason: err.to_string(),
                });
            }
        };
        if body.trim().eq_ignore_ascii_case("fails.") {
            tracing::error!(info_hash = %info_hash, "qBittorrent 拒绝了添加请求");
            return Err(DownloaderError::Message(
                "qBittorrent rejected the add".into(),
            ));
        }
        tracing::info!(info_hash = %info_hash, tag = %tags, "种子已成功添加到 qBittorrent");
        Ok(crate::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: info_hash.to_string(),
        })
    }
}

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn write_torrent(tag: &str, bytes: &[u8]) -> Result<Self, DownloaderError> {
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let safe_tag: String = tag
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = std::env::temp_dir().join(format!(
            "cm-torrent-{}-{n}-{safe_tag}.torrent",
            std::process::id()
        ));
        std::fs::write(&path, bytes)?;
        Ok(Self { path })
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn build_qbit_tags(options: &crate::SubmitOptions) -> Result<String, DownloaderError> {
    if options.ownership_tag.contains(',') || options.ownership_tag.chars().any(char::is_control) {
        return Err(DownloaderError::Message("invalid ownership tag".into()));
    }
    let mut tags = vec![
        crate::TASK_TAG.to_string(),
        options.ownership_tag.trim().to_string(),
    ];
    for extra in &options.extra_tags {
        if extra.contains(',') || extra.chars().any(char::is_control) {
            return Err(DownloaderError::Message(format!("invalid tag `{extra}`")));
        }
        let t = extra.trim();
        if !t.is_empty() && !tags.iter().any(|existing| existing == t) {
            tags.push(t.to_string());
        }
    }
    Ok(tags.join(","))
}

fn is_post_send_ureq_error(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Io(io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::TimedOut
        ),
        ureq::Error::Timeout(_) => true,
        ureq::Error::StatusCode(code) => matches!(code, 502 | 504),
        _ => false,
    }
}

enum SendOrBuildErr {
    Build(DownloaderError),
    Http(ureq::Error),
}

fn build_torrent_file_part(
    path: &Path,
) -> Result<ureq::unversioned::multipart::Part<'_>, DownloaderError> {
    ureq::unversioned::multipart::Part::file(path).map_err(DownloaderError::from)
}

fn classify_qbit_send_err(err: ureq::Error) -> Result<crate::ClientSubmitResult, DownloaderError> {
    if is_post_send_ureq_error(&err) {
        Ok(crate::ClientSubmitResult::UnknownOutcome {
            reason: err.to_string(),
        })
    } else {
        Err(http_err(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_torrent_file_part_propagates_io_error_for_missing_file() {
        let missing = std::env::temp_dir().join(format!(
            "cm-nonexistent-{}-{}.torrent",
            std::process::id(),
            999_999
        ));
        let err = build_torrent_file_part(&missing)
            .err()
            .expect("missing file must error");
        assert!(
            matches!(err, DownloaderError::Io(_)),
            "unexpected error: {err:?}"
        );
    }
}
