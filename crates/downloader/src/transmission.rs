#[path = "transmission_boost.rs"]
mod boost_ops;
#[path = "transmission_helpers.rs"]
mod helpers;
use helpers::{RpcResponse, base64_basic, extract_item_files, is_transmission_post_send_err};

use std::path::PathBuf;

use base64::Engine as _;
use domain::Torrent;
use serde_json::{Value, json};
use ureq::typestate::WithBody;

use crate::{
    ClientSubmitResult, ClientTaskQuery, Downloader, DownloaderError, PathMap, SubmitOptions,
    SubmitPayload, agent_for_url,
};

#[derive(Clone, PartialEq, Eq)]
pub struct TransmissionConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Save-path prefix mappings (container path -> host path).
    pub path_maps: Vec<PathMap>,
}

/// 手写 `Debug`：`password` 是下载器凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?config)` 都会把 Transmission
/// 口令打进日志与前端"系统日志"。`None`/`Some("<redacted>")` 仍能看出是否配置了
/// 口令，其余字段保留原样，便于诊断。
impl std::fmt::Debug for TransmissionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransmissionConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("path_maps", &self.path_maps)
            .finish()
    }
}

pub struct TransmissionDownloader {
    config: TransmissionConfig,
    session: parking_lot::Mutex<Option<String>>,
    identities: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    labels_supported: bool,
}

impl TransmissionDownloader {
    /// Review H-3: the task carries `CLAIMED_TAG` (unknown counts as claimed).
    fn hash_claimed(&self, hash: &str) -> bool {
        match self.rpc(
            "torrent-get",
            json!({ "ids": [hash], "fields": ["labels"] }),
        ) {
            Ok(got) => got["torrents"][0]["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|v| v.as_str() == Some(crate::CLAIMED_TAG)),
            Err(error) => {
                tracing::warn!(%hash, %error, "无法确认任务是否为认领的用户任务，按认领处理（不删文件）");
                true
            }
        }
    }

    pub fn connect(config: TransmissionConfig) -> Result<Self, DownloaderError> {
        tracing::debug!(url = %config.url, "正在连接 Transmission RPC");
        let mut dl = Self {
            config,
            session: parking_lot::Mutex::new(None),
            identities: parking_lot::Mutex::new(std::collections::HashMap::new()),
            labels_supported: false,
        };
        let session = dl.rpc("session-get", json!({})).map_err(|e| {
            tracing::error!(url = %dl.config.url, error = %e, "连接 Transmission 失败");
            e
        })?;
        dl.labels_supported = session["rpc-version"].as_u64().is_some_and(|v| v >= 16);
        tracing::info!(url = %dl.config.url, "Transmission RPC 连接成功");
        Ok(dl)
    }

    fn live_hash(&self, hash: &str) -> Result<Option<String>, DownloaderError> {
        let rows = self.rpc("torrent-get", json!({"fields": ["hashString"]}))?;
        let torrents = rows["torrents"]
            .as_array()
            .ok_or_else(|| crate::owned::unproven("invalid Transmission task list"))?;
        let hashes = torrents
            .iter()
            .filter(|row| {
                row["hashString"]
                    .as_str()
                    .is_some_and(|live| live.eq_ignore_ascii_case(hash))
            })
            .filter_map(|row| row["hashString"].as_str().map(str::to_string))
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    fn labeled_hash(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if !self.labels_supported {
            return Ok(None);
        }
        let rows = self.rpc("torrent-get", json!({"fields": ["hashString", "labels"]}))?;
        let torrents = rows["torrents"]
            .as_array()
            .ok_or_else(|| crate::owned::unproven("invalid Transmission task list"))?;
        let tag = crate::ownership_tag(&torrent.enclosure);
        let hashes = torrents
            .iter()
            .filter(|row| {
                row["labels"]
                    .as_array()
                    .is_some_and(|labels| labels.iter().any(|label| label.as_str() == Some(&tag)))
            })
            .filter_map(|row| row["hashString"].as_str().map(str::to_string))
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    fn rpc(&self, method: &str, arguments: Value) -> Result<Value, DownloaderError> {
        let payload = json!({ "method": method, "arguments": arguments }).to_string();
        let response = self
            .send(&payload)
            .map_err(|e| DownloaderError::Message(e.to_string()))?;
        if let Some(session) = response
            .headers()
            .get("X-Transmission-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock() = Some(session.to_string());
        }
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| DownloaderError::Message(err.to_string()))?;
        let parsed: RpcResponse =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        if parsed.result != "success" {
            return Err(DownloaderError::Message(parsed.result));
        }
        Ok(parsed.arguments)
    }

    fn send(&self, payload: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let agent = agent_for_url(&self.config.url, false);
        let make_req = |session_id: Option<&str>| {
            let mut request = agent.post(&self.config.url);
            if let (Some(user), Some(pass)) = (&self.config.username, &self.config.password) {
                request = request.header(
                    "Authorization",
                    format!("Basic {}", base64_basic(user, pass)),
                );
            }
            if let Some(session) = session_id {
                request = request.header("X-Transmission-Session-Id", session);
            }
            request.header("Content-Type", "application/json")
        };

        let current_session = self.session.lock().clone();
        let resp = make_req(current_session.as_deref()).send(payload)?;

        if resp.status().as_u16() == 409 {
            return self.retry_after_409(&resp, payload, &make_req);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            tracing::error!(url = %self.config.url, %status, "Transmission 响应非成功状态");
            return Err(ureq::Error::StatusCode(status.as_u16()));
        }
        Ok(resp)
    }

    fn retry_after_409<F>(
        &self,
        resp: &ureq::http::Response<ureq::Body>,
        payload: &str,
        make_req: &F,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>
    where
        F: Fn(Option<&str>) -> ureq::RequestBuilder<WithBody>,
    {
        let Some(new_token) = resp
            .headers()
            .get("X-Transmission-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
        else {
            tracing::error!(url = %self.config.url, "Transmission 409 响应缺少 X-Transmission-Session-Id");
            return Err(ureq::Error::StatusCode(409));
        };
        *self.session.lock() = Some(new_token.clone());
        let retry_resp = make_req(Some(&new_token)).send(payload)?;
        if retry_resp.status().as_u16() == 409 {
            tracing::error!(url = %self.config.url, "带新 Session ID 重试后 Transmission 仍然返回 409");
            return Err(ureq::Error::StatusCode(409));
        }
        if !retry_resp.status().is_success() {
            let status = retry_resp.status();
            tracing::error!(url = %self.config.url, %status, "Transmission 重试响应非成功状态");
            return Err(ureq::Error::StatusCode(status.as_u16()));
        }
        Ok(retry_resp)
    }

    fn fetch_task_row(&self, info_hash: &str) -> Result<Option<Value>, DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        let mut fields = vec!["id", "hashString", "error", "status"];
        if self.labels_supported {
            fields.push("labels");
        }
        let got = self.rpc(
            "torrent-get",
            json!({ "ids": [normalized], "fields": fields }),
        )?;
        let Some(items) = got["torrents"].as_array() else {
            return Ok(None);
        };
        Ok(items
            .iter()
            .find(|row| {
                row["hashString"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&normalized))
            })
            .cloned())
    }
}

impl Downloader for TransmissionDownloader {
    fn supports_labels(&self) -> bool {
        self.labels_supported
    }

    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        if let Some(hash) = crate::owned::normalize_hash(hash) {
            self.identities
                .lock()
                .entry(torrent.enclosure.clone())
                .or_insert(hash);
        }
        Ok(())
    }

    fn submit_verified(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        let info_hash = payload.info_hash().trim().to_ascii_lowercase();
        boost_ops::validate_tr_submit_options(&info_hash, options)?;
        if let Some(existing) = self.check_existing_task_before_add(&info_hash, options)? {
            return Ok(existing);
        }
        let arguments = self.build_torrent_add_arguments(payload, options);
        let rpc_payload = json!({ "method": "torrent-add", "arguments": arguments }).to_string();
        let response = match self.send(&rpc_payload) {
            Ok(resp) => resp,
            Err(err) => {
                return if is_transmission_post_send_err(&err) {
                    Ok(ClientSubmitResult::UnknownOutcome {
                        reason: err.to_string(),
                    })
                } else {
                    Err(DownloaderError::Message(err.to_string()))
                };
            }
        };
        self.parse_torrent_add_response(response, &info_hash, options)
    }

    fn query_task_status(
        &self,
        info_hash: &str,
        ownership_tag: &str,
    ) -> Result<ClientTaskQuery, DownloaderError> {
        let Some(row) = self.fetch_task_row(info_hash)? else {
            return Ok(ClientTaskQuery::default());
        };
        let owned_by_tag = self.labels_supported
            && !ownership_tag.trim().is_empty()
            && row["labels"]
                .as_array()
                .is_some_and(|ls| ls.iter().any(|v| v.as_str() == Some(ownership_tag)));
        let broken = row["error"].as_i64().unwrap_or(0) != 0;
        let tr_status = row["status"].as_i64().unwrap_or(-1);
        let paused = tr_status == 0;
        let state = match tr_status {
            0 => Some("stopped".into()),
            1 | 2 => Some("checking".into()),
            3 | 4 => Some("downloading".into()),
            5 | 6 => Some("seeding".into()),
            _ => None,
        };
        Ok(ClientTaskQuery {
            exists: true,
            info_hash: row["hashString"].as_str().map(|s| s.to_ascii_lowercase()),
            owned_by_tag,
            broken,
            state,
            paused,
        })
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if let Some(hash) = crate::magnet_info_hash(&torrent.enclosure) {
            return self.live_hash(&hash);
        }
        if let Some(hash) = self.identities.lock().get(&torrent.enclosure).cloned() {
            return self.live_hash(&hash);
        }
        self.labeled_hash(torrent)
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        let Some(hash) = self.owned_identity(torrent)? else {
            return Ok(None);
        };
        let arguments = self.rpc(
            "torrent-get",
            json!({ "ids": [hash.clone()], "fields": ["id", "hashString", "files"] }),
        )?;
        let Some(item) = arguments
            .get("torrents")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("hashString")
                        .and_then(Value::as_str)
                        .is_some_and(|h| h.eq_ignore_ascii_case(&hash))
                })
            })
        else {
            return Ok(None);
        };
        let Some(listed) = item
            .get("files")
            .and_then(Value::as_array)
            .filter(|files| !files.is_empty())
        else {
            return Ok(None); // metadata not received yet
        };
        let names = listed
            .iter()
            .map(|file| file.get("name").and_then(Value::as_str).unwrap_or(""));
        if crate::selection_drops_all_videos(names, keep) {
            tracing::warn!(hash = %hash, "选择性下载会跳过全部视频文件，放弃筛选、整包下载");
            return Ok(Some(0));
        }
        let skipped: Vec<usize> = Some(listed)
            .map(|files| {
                files
                    .iter()
                    .enumerate()
                    .filter(|(_, file)| {
                        !keep(file.get("name").and_then(Value::as_str).unwrap_or(""))
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        if skipped.is_empty() {
            return Ok(Some(0));
        }
        let id = item.get("id").cloned().unwrap_or(Value::Null);
        tracing::info!(hash = %hash, count = skipped.len(), "Transmission 跳过不需要的文件");
        self.rpc(
            "torrent-set",
            json!({ "ids": [id], "files-unwanted": skipped }),
        )?;
        Ok(Some(skipped.len()))
    }

    fn endpoint(&self) -> Option<String> {
        Some(format!(
            "transmission|{}",
            self.config.url.trim_end_matches('/')
        ))
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        Ok(match self.owned_identity(torrent)? {
            Some(hash) => self.hash_claimed(&hash),
            None => false,
        })
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        let known = self.identities.lock().contains_key(&torrent.enclosure);
        let Some(hash) = crate::owned::owned_removal_target(
            self.owned_identity(torrent),
            &torrent.enclosure,
            known,
        )?
        else {
            return Ok(());
        };
        let delete_files = delete_files && !self.hash_claimed(&hash);
        tracing::info!(torrent = %torrent.title, hash = %hash, delete_files, "从 Transmission 移除已证实归属的种子");
        self.rpc(
            "torrent-remove",
            json!({ "ids": [hash], "delete-local-data": delete_files }),
        )?;
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        let delete_files = delete_files && !self.hash_claimed(info_hash);
        tracing::info!(hash = %info_hash, delete_files, "从 Transmission 按已证实哈希删除任务");
        self.rpc(
            "torrent-remove",
            json!({ "ids": [info_hash], "delete-local-data": delete_files }),
        )?;
        Ok(())
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "从 Transmission 暂停任务");
        self.rpc("torrent-stop", json!({ "ids": [info_hash] }))?;
        Ok(())
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "从 Transmission 恢复任务");
        self.rpc("torrent-start", json!({ "ids": [info_hash] }))?;
        Ok(())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        let (files, done) = self.collectable_files(torrent)?;
        Ok(if done { files } else { Vec::new() })
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        let arguments = self.rpc(
            "torrent-get",
            json!({
                "fields": ["id", "name", "hashString", "sizeWhenDone", "percentDone", "downloadDir", "files", "fileStats"]
            }),
        )?;
        let empty = Vec::new();
        let torrents = arguments
            .get("torrents")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let Some(owned_hash) = self.owned_identity(torrent)? else {
            return Ok((Vec::new(), false));
        };
        let matched: Vec<&Value> = torrents
            .iter()
            .filter(|item| {
                item.get("hashString")
                    .and_then(Value::as_str)
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&owned_hash))
            })
            .collect();
        if matched.is_empty() {
            return Ok((Vec::new(), false));
        }
        if matched.len() > 1 {
            tracing::error!(torrent = %torrent.title, matched_count = matched.len(), "匹配到多个任务存在歧义，拒绝收集文件");
            return Err(DownloaderError::Message(
                "ambiguous Transmission torrent identity".into(),
            ));
        }
        let target_item = matched[0];
        // percentDone covers the wanted files only.
        let done = target_item
            .get("percentDone")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            >= 1.0;
        let mut files = Vec::new();
        extract_item_files(target_item, &self.config.path_maps, &mut files);
        Ok((files, done))
    }

    fn task_snapshots(&self) -> Result<Vec<crate::TaskSnapshot>, DownloaderError> {
        let mut fields = vec![
            "name",
            "hashString",
            "totalSize",
            "percentDone",
            "rateDownload",
            "rateUpload",
            "uploadedEver",
            "downloadedEver",
            "status",
        ];
        if self.labels_supported {
            fields.push("labels");
        }
        let response = self
            .rpc("torrent-get", json!({"fields": fields}))
            .map_err(|error| {
                tracing::error!(%error, "读取 Transmission 任务快照失败");
                error
            })?;
        let items = response["torrents"].as_array().ok_or_else(|| {
            tracing::error!("Transmission 任务快照缺少 torrents 数组");
            DownloaderError::Message("invalid Transmission task snapshot list".into())
        })?;
        let mut snapshots = Vec::new();
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let info_hash = item
                .get("hashString")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let size_bytes = item.get("totalSize").and_then(Value::as_u64).unwrap_or(0);
            let tag = item["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|label| *label == crate::TASK_TAG)
                .unwrap_or("")
                .to_string();
            let owned_tags = crate::owned_tags_from(
                item["labels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let progress = item
                .get("percentDone")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let download_speed = item
                .get("rateDownload")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let upload_speed = item.get("rateUpload").and_then(Value::as_u64).unwrap_or(0);
            let downloaded_bytes = item
                .get("downloadedEver")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let uploaded_bytes = item
                .get("uploadedEver")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let tr_status = item.get("status").and_then(Value::as_i64).unwrap_or(0);
            // Transmission status: 0=stopped, 1=check_wait, 2=check, 3=download_wait, 4=download, 5=seed_wait, 6=seed
            let state = match tr_status {
                0 if progress >= 1.0 => "pausedUP".into(),
                0 => "pausedDL".into(),
                1 | 2 => "checkingDL".into(),
                3 | 4 => "downloading".into(),
                5 | 6 => "uploading".into(),
                _ => "unknown".into(),
            };
            snapshots.push(crate::TaskSnapshot {
                info_hash,
                name,
                progress,
                size_bytes,
                download_speed,
                upload_speed,
                downloaded_bytes,
                uploaded_bytes,
                state,
                tag,
                owned_tags,
            });
        }
        Ok(snapshots)
    }

    fn task_files_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<crate::TaskFileSet>, DownloaderError> {
        self.tr_task_files_by_hash(info_hash)
    }

    fn set_task_tags(&self, info_hash: &str, tags: &[String]) -> Result<(), DownloaderError> {
        self.tr_set_task_tags(info_hash, tags)
    }

    fn set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        self.tr_set_task_limits(info_hash, upload_limit, download_limit)
    }

    fn disk_free_space(&self) -> Result<crate::DiskSpaceInfo, DownloaderError> {
        self.tr_disk_free_space()
    }
}

impl TransmissionDownloader {
    fn check_existing_task_before_add(
        &self,
        info_hash: &str,
        options: &SubmitOptions,
    ) -> Result<Option<ClientSubmitResult>, DownloaderError> {
        let Some(row) = self.fetch_task_row(info_hash)? else {
            return Ok(None);
        };
        let hash = info_hash.to_string();
        if self.labels_supported {
            let expected_tag = options.ownership_tag.trim();
            let labels = row["labels"].as_array();
            let exact_owned = labels.is_some_and(|ls| {
                ls.iter()
                    .filter_map(Value::as_str)
                    .any(|t| t.trim() == expected_tag)
            });
            let instance_owned = exact_owned
                || labels.is_some_and(|ls| {
                    ls.iter()
                        .filter_map(Value::as_str)
                        .any(|t| crate::is_instance_owned_tag(t, expected_tag))
                });
            if !instance_owned {
                return Ok(Some(ClientSubmitResult::ForeignDuplicate {
                    info_hash: hash,
                }));
            }
            if row["error"].as_i64().unwrap_or(0) != 0 {
                return Ok(Some(ClientSubmitResult::ExistingBroken { info_hash: hash }));
            }
            if exact_owned {
                self.tr_merge_missing_labels(info_hash, labels, options)?;
                if row["status"].as_i64() == Some(0) && !options.paused {
                    self.resume_task(info_hash)?;
                }
                return Ok(Some(ClientSubmitResult::Added {
                    already_owned: true,
                    info_hash: hash,
                }));
            }
            return Ok(Some(ClientSubmitResult::ForeignDuplicate {
                info_hash: hash,
            }));
        }
        if row["error"].as_i64().unwrap_or(0) != 0 {
            return Ok(Some(ClientSubmitResult::ExistingBroken { info_hash: hash }));
        }
        Ok(Some(ClientSubmitResult::ForeignDuplicate {
            info_hash: hash,
        }))
    }

    fn build_torrent_add_arguments(
        &self,
        payload: &SubmitPayload,
        options: &SubmitOptions,
    ) -> Value {
        let mut arguments = json!({ "paused": options.paused });
        match payload {
            SubmitPayload::TorrentBytes { bytes, .. } => {
                arguments["metainfo"] =
                    json!(base64::engine::general_purpose::STANDARD.encode(bytes));
            }
            SubmitPayload::Magnet { uri, .. } => {
                arguments["filename"] = json!(uri);
            }
        }
        if let Some(save_path) = options
            .save_path
            .as_deref()
            .filter(|p| !p.trim().is_empty())
        {
            arguments["download-dir"] = json!(save_path);
        }
        if self.labels_supported {
            let mut labels = vec![
                crate::TASK_TAG.to_string(),
                options.ownership_tag.trim().to_string(),
            ];
            for extra in &options.extra_tags {
                let t = extra.trim();
                if !t.is_empty() && !labels.iter().any(|l| l == t) {
                    labels.push(t.to_string());
                }
            }
            arguments["labels"] = json!(labels);
        }
        arguments
    }

    fn parse_torrent_add_response(
        &self,
        response: ureq::http::Response<ureq::Body>,
        info_hash: &str,
        options: &SubmitOptions,
    ) -> Result<ClientSubmitResult, DownloaderError> {
        if let Some(session) = response
            .headers()
            .get("X-Transmission-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock() = Some(session.to_string());
        }
        let body = match response.into_body().read_to_string() {
            Ok(b) => b,
            Err(err) => {
                return Ok(ClientSubmitResult::UnknownOutcome {
                    reason: err.to_string(),
                });
            }
        };
        let parsed: RpcResponse = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(err) => {
                return Ok(ClientSubmitResult::UnknownOutcome {
                    reason: err.to_string(),
                });
            }
        };
        if parsed.result != "success" {
            return Err(DownloaderError::Message(parsed.result));
        }
        if parsed.arguments.get("torrent-duplicate").is_some() {
            return Ok(ClientSubmitResult::ForeignDuplicate {
                info_hash: info_hash.to_string(),
            });
        }
        if let Some(added) = parsed.arguments.get("torrent-added") {
            let resolved_hash = added["hashString"]
                .as_str()
                .and_then(crate::owned::normalize_hash)
                .unwrap_or_else(|| info_hash.to_string());
            self.apply_transmission_limits(&resolved_hash, options);
            return Ok(ClientSubmitResult::Added {
                already_owned: false,
                info_hash: resolved_hash,
            });
        }
        Ok(ClientSubmitResult::UnknownOutcome {
            reason: "transmission torrent-add returned empty arguments".into(),
        })
    }

    fn apply_transmission_limits(&self, info_hash: &str, options: &SubmitOptions) {
        if options.upload_limit.is_none() && options.download_limit.is_none() {
            return;
        }
        let mut set_args = json!({ "ids": [info_hash] });
        if let Some(ul) = options.upload_limit {
            if ul == 0 {
                set_args["uploadLimited"] = json!(false);
            } else {
                set_args["uploadLimited"] = json!(true);
                set_args["uploadLimit"] = json!((ul / 1024).max(1));
            }
        }
        if let Some(dl) = options.download_limit {
            if dl == 0 {
                set_args["downloadLimited"] = json!(false);
            } else {
                set_args["downloadLimited"] = json!(true);
                set_args["downloadLimit"] = json!((dl / 1024).max(1));
            }
        }
        if let Err(err) = self.rpc("torrent-set", set_args) {
            tracing::warn!(info_hash = %info_hash, error = %err, "Transmission torrent-set 限速失败");
        }
    }
}
