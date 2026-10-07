use downloader::{PathMap, QbitConfig, TransmissionConfig, parse_path_maps};
use serde::Deserialize;

use crate::store::{Store, StoreError};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloaderEnv {
    pub qb_url: Option<String>,
    pub qb_user: Option<String>,
    pub qb_pass: Option<String>,
    pub qb_category: Option<String>,
    /// Save-path prefix mappings for qBittorrent (container -> host).
    pub qb_path_maps: Vec<PathMap>,
    /// Save-path prefix mappings for Transmission (container -> host).
    pub tr_path_maps: Vec<PathMap>,
}

impl DownloaderEnv {
    pub fn from_os() -> Self {
        Self {
            qb_url: nonempty("CRAWLER_MEDIA_QB_URL"),
            qb_user: nonempty("CRAWLER_MEDIA_QB_USER"),
            qb_pass: nonempty("CRAWLER_MEDIA_QB_PASS"),
            qb_category: nonempty("CRAWLER_MEDIA_QB_CATEGORY"),
            qb_path_maps: nonempty("CRAWLER_MEDIA_QB_PATH_MAP")
                .map(|raw| parse_path_maps(&raw))
                .unwrap_or_default(),
            tr_path_maps: nonempty("CRAWLER_MEDIA_TR_PATH_MAP")
                .map(|raw| parse_path_maps(&raw))
                .unwrap_or_default(),
        }
    }

    /// Build downloader overrides from the already-parsed ServerConfig.
    /// This preserves `_FILE` secret resolution (notably QB_PASS_FILE), unlike
    /// `from_os()` which intentionally only sees raw environment variables.
    pub fn from_server_config(config: &crate::config::ServerConfig) -> Self {
        Self {
            qb_url: config.qb_url.clone(),
            qb_user: config.qb_user.clone(),
            qb_pass: config.qb_pass.clone(),
            qb_category: config.qb_category.clone(),
            qb_path_maps: config
                .qb_path_map_raw
                .as_deref()
                .map(parse_path_maps)
                .unwrap_or_default(),
            tr_path_maps: config
                .tr_path_map_raw
                .as_deref()
                .map(parse_path_maps)
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChosenDownloader {
    Qbittorrent(QbitConfig),
    Transmission(TransmissionConfig),
    Memory,
}

pub fn choose_downloader(
    store: &Store,
    env: &DownloaderEnv,
) -> Result<ChosenDownloader, StoreError> {
    if let (Some(url), Some(username), Some(password)) =
        (env.qb_url.clone(), env.qb_user.clone(), env.qb_pass.clone())
    {
        return Ok(ChosenDownloader::Qbittorrent(QbitConfig {
            url,
            username,
            password,
            category: env.qb_category.clone(),
            path_maps: env.qb_path_maps.clone(),
        }));
    }
    if let Some(row) = store.default_downloader()? {
        return Ok(from_row(row, env));
    }
    let Some(raw) = store.get_setting("downloader")? else {
        return Ok(ChosenDownloader::Memory);
    };
    let saved: SavedDownloader = serde_json::from_str(&raw)?;
    Ok(from_saved(saved, env))
}

/// Convert a stored downloader row to a ChosenDownloader config using env overrides.
/// Exposed so the submit handler can connect to a caller-specified downloader.
pub fn row_to_chosen(row: crate::store::DownloaderRow, env: &DownloaderEnv) -> ChosenDownloader {
    from_row(row, env)
}

fn from_row(row: crate::store::DownloaderRow, env: &DownloaderEnv) -> ChosenDownloader {
    match row.kind.as_str() {
        "qbittorrent" => {
            let username = row.username.unwrap_or_default();
            let password = row.password.unwrap_or_default();
            if row.url.is_empty() || username.is_empty() || password.is_empty() {
                return ChosenDownloader::Memory;
            }
            // Env path_maps override DB path_maps (if env is set).
            let path_maps = if env.qb_path_maps.is_empty() {
                row.path_maps
            } else {
                env.qb_path_maps.clone()
            };
            ChosenDownloader::Qbittorrent(QbitConfig {
                url: row.url,
                username,
                password,
                category: row.category,
                path_maps,
            })
        }
        "transmission" => {
            if row.url.is_empty() {
                return ChosenDownloader::Memory;
            }
            let path_maps = if env.tr_path_maps.is_empty() {
                row.path_maps
            } else {
                env.tr_path_maps.clone()
            };
            ChosenDownloader::Transmission(TransmissionConfig {
                url: row.url,
                username: row.username,
                password: row.password,
                path_maps,
            })
        }
        _ => ChosenDownloader::Memory,
    }
}

fn from_saved(saved: SavedDownloader, env: &DownloaderEnv) -> ChosenDownloader {
    match saved.kind.as_str() {
        "qbittorrent" => {
            let username = saved.username.unwrap_or_default();
            let password = saved.password.unwrap_or_default();
            if saved.url.is_empty() || username.is_empty() || password.is_empty() {
                return ChosenDownloader::Memory;
            }
            ChosenDownloader::Qbittorrent(QbitConfig {
                url: saved.url,
                username,
                password,
                category: saved.category,
                path_maps: env.qb_path_maps.clone(),
            })
        }
        "transmission" => {
            if saved.url.is_empty() {
                return ChosenDownloader::Memory;
            }
            ChosenDownloader::Transmission(TransmissionConfig {
                url: saved.url,
                username: saved.username,
                password: saved.password,
                path_maps: env.tr_path_maps.clone(),
            })
        }
        _ => ChosenDownloader::Memory,
    }
}

fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[derive(Deserialize)]
struct SavedDownloader {
    kind: String,
    url: String,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    category: Option<String>,
}
