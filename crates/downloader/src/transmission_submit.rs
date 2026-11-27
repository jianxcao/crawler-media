use base64::Engine as _;
use serde_json::{Value, json};

use super::{RpcResponse, TransmissionDownloader};
use crate::{ClientSubmitResult, Downloader, DownloaderError, SubmitOptions, SubmitPayload};

impl TransmissionDownloader {
    pub(super) fn check_existing_task_before_add(
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

    pub(super) fn build_torrent_add_arguments(
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

    pub(super) fn parse_torrent_add_response(
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
