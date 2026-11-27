use std::path::Path;

use serde_json::{Value, json};

use crate::{
    DiskSpaceInfo, DiskSpaceStatus, DownloaderError, SubmitOptions, TaskFileSet, apply_maps,
};

use super::TransmissionDownloader;

fn is_system_label(tag: &str) -> bool {
    crate::is_reserved_system_tag(tag)
}

pub(super) fn validate_tr_submit_options(
    info_hash: &str,
    options: &SubmitOptions,
) -> Result<(), DownloaderError> {
    if info_hash.is_empty() || options.ownership_tag.trim().is_empty() {
        return Err(DownloaderError::Message(
            "info_hash and ownership_tag must not be empty".into(),
        ));
    }
    if options.ownership_tag.contains(',')
        || options.ownership_tag.chars().any(char::is_control)
        || options
            .extra_tags
            .iter()
            .any(|t| t.contains(',') || t.chars().any(char::is_control))
    {
        return Err(DownloaderError::Message(
            "tags must not contain commas or control characters".into(),
        ));
    }
    if let Some(cat) = options.category.as_deref()
        && let Err(reason) = crate::validate_category_name(cat)
    {
        return Err(DownloaderError::Message(reason.into()));
    }
    Ok(())
}

impl TransmissionDownloader {
    pub(super) fn tr_merge_missing_labels(
        &self,
        info_hash: &str,
        existing_labels: Option<&Vec<Value>>,
        options: &SubmitOptions,
    ) -> Result<(), DownloaderError> {
        let mut merged: Vec<String> = existing_labels
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let before_len = merged.len();
        for candidate in [crate::TASK_TAG, options.ownership_tag.trim()]
            .into_iter()
            .chain(options.extra_tags.iter().map(|s| s.trim()))
        {
            if !candidate.is_empty() && !merged.iter().any(|cur| cur == candidate) {
                merged.push(candidate.to_string());
            }
        }
        if merged.len() > before_len {
            self.rpc(
                "torrent-set",
                json!({ "ids": [info_hash], "labels": merged }),
            )?;
        }
        Ok(())
    }

    pub(super) fn tr_task_files_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<TaskFileSet>, DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        let mut fields = vec!["hashString", "downloadDir", "totalSize", "files"];
        if self.labels_supported {
            fields.push("labels");
        }
        let got = self.rpc(
            "torrent-get",
            json!({ "ids": [normalized], "fields": fields }),
        )?;
        let Some(item) = got
            .get("torrents")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|r| {
                    r.get("hashString")
                        .and_then(Value::as_str)
                        .is_some_and(|h| h.eq_ignore_ascii_case(&normalized))
                })
            })
        else {
            return Ok(None);
        };
        let save_path = item
            .get("downloadDir")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tags: Vec<String> = item["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let claimed_by_app = tags.iter().any(|t| t == crate::CLAIMED_TAG);
        let mut files = Vec::new();
        let mut file_sum: u64 = 0;
        if let Some(listed) = item.get("files").and_then(Value::as_array) {
            for file in listed {
                let len = file.get("length").and_then(Value::as_u64).unwrap_or(0);
                file_sum = file_sum.saturating_add(len);
                if let Some(name) = file.get("name").and_then(Value::as_str)
                    && !name.is_empty()
                {
                    let joined = Path::new(&save_path).join(name);
                    files.push(apply_maps(&joined, &self.config.path_maps));
                }
            }
        }
        let total_size = item.get("totalSize").and_then(Value::as_u64).unwrap_or(0);
        let total_bytes = if total_size > 0 { total_size } else { file_sum };
        Ok(Some(TaskFileSet {
            save_path,
            files,
            total_bytes,
            claimed_by_app,
            tags,
            category: None,
        }))
    }

    pub(super) fn tr_set_task_tags(
        &self,
        info_hash: &str,
        tags: &[String],
    ) -> Result<(), DownloaderError> {
        if !self.labels_supported {
            return Err(DownloaderError::Message(
                "Transmission RPC < 16 does not support labels".into(),
            ));
        }
        for raw in tags {
            if raw.contains(',') || raw.chars().any(char::is_control) {
                return Err(DownloaderError::Message(format!("invalid tag `{raw}`")));
            }
        }
        let normalized = info_hash.trim().to_ascii_lowercase();
        let Some(row) = self.fetch_task_row(&normalized)? else {
            return Err(DownloaderError::Message(format!(
                "Transmission task {normalized} not found"
            )));
        };
        let current: Vec<String> = row["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let mut merged: Vec<String> = current
            .iter()
            .filter(|t| is_system_label(t))
            .cloned()
            .collect();
        for t in tags {
            let trimmed = t.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_system_label(trimmed) && !current.iter().any(|c| c == trimmed) {
                return Err(DownloaderError::Message(format!(
                    "cannot inject unowned system tag `{trimmed}`"
                )));
            }
            if !merged.iter().any(|m| m == trimmed) {
                merged.push(trimmed.to_string());
            }
        }
        self.rpc(
            "torrent-set",
            json!({ "ids": [normalized], "labels": merged }),
        )?;
        Ok(())
    }

    pub(super) fn tr_set_task_limits(
        &self,
        info_hash: &str,
        upload_limit: Option<u64>,
        download_limit: Option<u64>,
    ) -> Result<(), DownloaderError> {
        let normalized = info_hash.trim().to_ascii_lowercase();
        let mut args = json!({ "ids": [normalized] });
        if let Some(ul) = upload_limit {
            if ul == 0 {
                args["uploadLimited"] = json!(false);
            } else {
                args["uploadLimited"] = json!(true);
                args["uploadLimit"] = json!((ul / 1024).max(1));
            }
        }
        if let Some(dl) = download_limit {
            if dl == 0 {
                args["downloadLimited"] = json!(false);
            } else {
                args["downloadLimited"] = json!(true);
                args["downloadLimit"] = json!((dl / 1024).max(1));
            }
        }
        self.rpc("torrent-set", args)?;
        Ok(())
    }

    pub(super) fn tr_disk_free_space(&self) -> Result<DiskSpaceInfo, DownloaderError> {
        let session = self.rpc("session-get", json!({}))?;
        let Some(dir) = session.get("download-dir").and_then(Value::as_str) else {
            return Ok(DiskSpaceInfo {
                available_bytes: None,
                root_path: self.config.path_maps.first().map(|m| m.to.clone()),
                status: DiskSpaceStatus::Unknown,
            });
        };
        let free = self.rpc("free-space", json!({ "path": dir }))?;
        let bytes = free.get("size-bytes").and_then(Value::as_u64);
        let mapped = apply_maps(Path::new(dir), &self.config.path_maps);
        Ok(DiskSpaceInfo {
            available_bytes: bytes,
            root_path: Some(mapped),
            status: if bytes.is_some() {
                DiskSpaceStatus::Available
            } else {
                DiskSpaceStatus::Unknown
            },
        })
    }
}
