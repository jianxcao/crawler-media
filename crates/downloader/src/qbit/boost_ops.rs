use std::path::Path;

use crate::{DiskSpaceInfo, DiskSpaceStatus, DownloaderError, TaskFileSet, apply_maps};

use super::{QbitDownloader, form_enc, join};

fn is_system_tag(tag: &str) -> bool {
    crate::is_reserved_system_tag(tag)
}

impl QbitDownloader {
    pub(super) fn check_existing_qbit_task(
        &self,
        info_hash: &str,
        options: &crate::SubmitOptions,
        desired_tags_csv: &str,
    ) -> Result<Option<crate::ClientSubmitResult>, DownloaderError> {
        let Some(info) = self.find_info_by_hash(info_hash)? else {
            return Ok(None);
        };
        let hash = info_hash.to_string();
        let expected_tag = options.ownership_tag.trim();
        let exact_owned = info
            .tags
            .split(',')
            .map(str::trim)
            .any(|t| t == expected_tag);
        let instance_owned = exact_owned
            || info
                .tags
                .split(',')
                .any(|t| crate::is_instance_owned_tag(t, expected_tag));
        if !instance_owned {
            return Ok(Some(crate::ClientSubmitResult::ForeignDuplicate {
                info_hash: hash,
            }));
        }
        let health = super::existing_task_health(&info.state, info.progress);
        if health == super::ExistingHealth::Broken {
            return Ok(Some(crate::ClientSubmitResult::ExistingBroken {
                info_hash: hash,
            }));
        }
        if exact_owned {
            let missing: Vec<&str> = desired_tags_csv
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .filter(|want| !info.tags.split(',').map(str::trim).any(|cur| cur == *want))
                .collect();
            if !missing.is_empty() {
                let url = join(&self.config.url, "/api/v2/torrents/addTags");
                let body = format!(
                    "hashes={}&tags={}",
                    form_enc(&info.hash),
                    form_enc(&missing.join(","))
                );
                let _ = self.post_form(&url, &body)?;
            }
            if health == super::ExistingHealth::Paused && !options.paused {
                crate::Downloader::resume_task(self, &info.hash)?;
            }
            return Ok(Some(crate::ClientSubmitResult::Added {
                already_owned: true,
                info_hash: hash,
            }));
        }
        Ok(Some(crate::ClientSubmitResult::ForeignDuplicate {
            info_hash: hash,
        }))
    }

    pub(super) fn qbit_task_files_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<TaskFileSet>, DownloaderError> {
        let Some(info) = self.find_info_by_hash(info_hash)? else {
            return Ok(None);
        };
        let listed = self.torrent_files(&info.hash)?;
        let tags: Vec<String> = info
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let claimed_by_app = tags.iter().any(|t| t == crate::CLAIMED_TAG);
        let files = listed
            .iter()
            .map(|f| {
                let p = Path::new(&info.save_path).join(&f.name);
                apply_maps(&p, &self.config.path_maps)
            })
            .collect();
        let file_sum: u64 = listed.iter().map(|f| f.size).sum();
        let total_bytes = if info.size > 0 { info.size } else { file_sum };
        let category = if info.category.trim().is_empty() {
            None
        } else {
            Some(info.category)
        };
        Ok(Some(TaskFileSet {
            save_path: info.save_path,
            files,
            total_bytes,
            claimed_by_app,
            tags,
            category,
        }))
    }

    pub(super) fn qbit_set_task_tags(
        &self,
        info_hash: &str,
        tags: &[String],
    ) -> Result<(), DownloaderError> {
        for raw in tags {
            if raw.contains(',') || raw.chars().any(char::is_control) {
                return Err(DownloaderError::Message(format!("invalid tag `{raw}`")));
            }
        }
        let normalized = info_hash.trim().to_ascii_lowercase();
        let Some(info) = self.find_info_by_hash(&normalized)? else {
            return Err(DownloaderError::Message(format!(
                "qBittorrent task {normalized} not found"
            )));
        };
        let current: Vec<String> = info
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let mut desired = Vec::with_capacity(tags.len());
        for raw in tags {
            let t = raw.trim();
            if t.is_empty() {
                continue;
            }
            if is_system_tag(t) && !current.iter().any(|c| c == t) {
                return Err(DownloaderError::Message(format!(
                    "cannot inject unowned system tag `{t}`"
                )));
            }
            desired.push(t.to_string());
        }

        let to_remove: Vec<String> = current
            .iter()
            .filter(|t| !is_system_tag(t) && !desired.iter().any(|d| d == *t))
            .cloned()
            .collect();
        let to_add: Vec<String> = desired
            .iter()
            .filter(|t| !current.contains(*t))
            .cloned()
            .collect();

        if !to_remove.is_empty() {
            let url = join(&self.config.url, "/api/v2/torrents/removeTags");
            let body = format!(
                "hashes={}&tags={}",
                form_enc(&normalized),
                form_enc(&to_remove.join(","))
            );
            let _ = self.post_form(&url, &body)?;
        }
        if !to_add.is_empty() {
            let url = join(&self.config.url, "/api/v2/torrents/addTags");
            let body = format!(
                "hashes={}&tags={}",
                form_enc(&normalized),
                form_enc(&to_add.join(","))
            );
            let _ = self.post_form(&url, &body)?;
        }
        Ok(())
    }

    pub(super) fn qbit_set_task_category(
        &self,
        info_hash: &str,
        category: &str,
    ) -> Result<(), DownloaderError> {
        if let Err(reason) = crate::validate_category_name(category) {
            return Err(DownloaderError::Message(reason.into()));
        }
        let normalized = info_hash.trim().to_ascii_lowercase();
        let url = join(&self.config.url, "/api/v2/torrents/setCategory");
        let body = format!(
            "hashes={}&category={}",
            form_enc(&normalized),
            form_enc(category)
        );
        match self.post_form(&url, &body) {
            Ok(_) => Ok(()),
            Err(err) if err.to_string().contains("409") && !category.trim().is_empty() => {
                let create_url = join(&self.config.url, "/api/v2/torrents/createCategory");
                let create_body = format!("category={}", form_enc(category));
                let _ = self.post_form(&create_url, &create_body)?;
                let _ = self.post_form(&url, &body)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub(super) fn qbit_set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        if let Some(ul) = upload_limit {
            let url = join(&self.config.url, "/api/v2/torrents/setUploadLimit");
            let body = format!("hashes={}&limit={ul}", form_enc(&normalized));
            let _ = self.post_form(&url, &body)?;
        }
        if let Some(dl) = download_limit {
            let url = join(&self.config.url, "/api/v2/torrents/setDownloadLimit");
            let body = format!("hashes={}&limit={dl}", form_enc(&normalized));
            let _ = self.post_form(&url, &body)?;
        }
        Ok(())
    }

    pub(super) fn qbit_disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        let url = join(&self.config.url, "/api/v2/sync/maindata?rid=0");
        let body = self.get(&url)?;
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        let available = parsed
            .get("server_state")
            .and_then(|s| s.get("free_space_on_disk"))
            .and_then(|v| v.as_u64());
        let root_path = self.config.path_maps.first().map(|m| m.to.clone());
        match available {
            Some(bytes) => Ok(DiskSpaceInfo {
                available_bytes: Some(bytes),
                root_path,
                status: DiskSpaceStatus::Available,
            }),
            None => Ok(DiskSpaceInfo {
                available_bytes: None,
                root_path,
                status: DiskSpaceStatus::Unknown,
            }),
        }
    }
}
