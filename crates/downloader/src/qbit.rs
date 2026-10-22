use std::path::{Path, PathBuf};

use domain::Torrent;
use serde::Deserialize;

use crate::{Downloader, DownloaderError, PathMap, agent_for_url, apply_maps};

mod downloader;

#[derive(Clone, PartialEq, Eq)]
pub struct QbitConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub category: Option<String>,
    /// Save-path prefix mappings (container path -> host path).
    pub path_maps: Vec<PathMap>,
}

/// 手写 `Debug`：`password` 是下载器凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?config)` 都会把 qBittorrent
/// 口令打进日志与前端"系统日志"。其余字段保留原样，便于诊断。
impl std::fmt::Debug for QbitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbitConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("category", &self.category)
            .field("path_maps", &self.path_maps)
            .finish()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::QbitConfig;

    #[test]
    fn debug_never_prints_the_password() {
        let config = QbitConfig {
            url: "http://127.0.0.1:8080".into(),
            username: "admin".into(),
            password: "qbit-password-secret".into(),
            category: Some("movies".into()),
            path_maps: Vec::new(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("qbit-password-secret"),
            "Debug 不得输出 password: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("admin"), "{rendered}");
    }
}

pub struct QbitDownloader {
    config: QbitConfig,
    /// Session cookie; refreshed by [`Self::relogin`] when the SID expires (D-H1).
    cookie: parking_lot::Mutex<String>,
    /// Serialises re-logins so concurrent 403s trigger a single login.
    login_lock: parking_lot::Mutex<()>,
}

fn http_agent() -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build();
    ureq::Agent::new_with_config(config)
}
fn read_body(response: ureq::http::Response<ureq::Body>) -> Result<String, DownloaderError> {
    response
        .into_body()
        .read_to_string()
        .map_err(|err| DownloaderError::Message(err.to_string()))
}

fn http_err(err: ureq::Error) -> DownloaderError {
    DownloaderError::Message(err.to_string())
}

fn join(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

fn form_enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct TorrentInfo {
    hash: String,
    progress: f64,
    save_path: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    uploaded: u64,
    #[serde(default)]
    category: String,
    /// Lifecycle string (`downloading`, `stalledDL`, `pausedDL`, `error`, …).
    #[serde(default)]
    state: String,
    #[serde(default)]
    downloaded: u64,
    #[serde(default)]
    dlspeed: u64,
    #[serde(default)]
    upspeed: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum ExistingHealth {
    Usable,
    Paused,
    Broken,
}

/// G-M2: classify an existing task's qBittorrent state (v4 `paused*`, v5 `stopped*`).
fn existing_task_health(state: &str, progress: f64) -> ExistingHealth {
    match state {
        "error" | "missingFiles" => ExistingHealth::Broken,
        "pausedDL" | "stoppedDL" if progress < 1.0 => ExistingHealth::Paused,
        _ => ExistingHealth::Usable,
    }
}

#[cfg(test)]
mod existing_health_tests {
    use super::{ExistingHealth, existing_task_health};

    #[test]
    fn maps_qbit_states() {
        assert_eq!(existing_task_health("error", 0.2), ExistingHealth::Broken);
        assert_eq!(
            existing_task_health("missingFiles", 1.0),
            ExistingHealth::Broken
        );
        assert_eq!(
            existing_task_health("pausedDL", 0.5),
            ExistingHealth::Paused
        );
        assert_eq!(
            existing_task_health("stoppedDL", 0.5),
            ExistingHealth::Paused
        );
        assert_eq!(
            existing_task_health("pausedUP", 1.0),
            ExistingHealth::Usable
        );
        assert_eq!(
            existing_task_health("downloading", 0.1),
            ExistingHealth::Usable
        );
    }
}

#[derive(Deserialize)]
struct TorrentFile {
    #[serde(default)]
    index: Option<usize>,
    name: String,
    progress: f64,
    /// 0 = do not download (F-1 selective files).
    #[serde(default = "default_priority")]
    priority: i64,
}

fn default_priority() -> i64 {
    1
}

/// Pick our marker out of a client's comma-separated tag list.
///
/// Recognizes the constant [`crate::TASK_TAG`] plus any legacy per-torrent
/// `crawler-media-<hash>` tags still present in the client from earlier
/// builds; anything else (foreign torrents) yields "".
fn our_tag(tags: &str) -> String {
    let items: Vec<&str> = tags.split(',').map(str::trim).collect();
    if items.iter().any(|item| *item == crate::TASK_TAG) {
        return crate::TASK_TAG.to_string();
    }
    items
        .into_iter()
        .find(|item| {
            item.starts_with("crawler-media-") && !item.starts_with("crawler-media-owned-")
        })
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::our_tag;

    #[test]
    fn our_tag_recognizes_the_constant_tag() {
        assert_eq!(our_tag("crawler-media"), "crawler-media");
    }

    #[test]
    fn our_tag_recognizes_legacy_per_torrent_tags() {
        assert_eq!(
            our_tag("other,crawler-media-0000000000000001"),
            "crawler-media-0000000000000001"
        );
        assert_eq!(
            our_tag("crawler-media,crawler-media-owned-abcd"),
            "crawler-media"
        );
    }

    #[test]
    fn our_tag_ignores_foreign_and_empty_lists() {
        assert_eq!(our_tag(""), "");
        assert_eq!(our_tag("other,boost"), "");
        assert_eq!(our_tag("  other  ,  boost  "), "");
    }
}
