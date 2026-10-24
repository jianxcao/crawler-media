use downloader::{PathMap, QbitConfig, TransmissionConfig, parse_path_maps};

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
    /// Test-only explicit injection. Production selection never returns this.
    Memory,
    /// A configured or missing production Downloader cannot be used.
    Unavailable,
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
    tracing::error!("生产环境没有可用的默认 Downloader");
    Err(StoreError::Missing("default Downloader".into()))
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
                tracing::error!(id = %row.id, "qBittorrent 配置不完整，拒绝内存假下载");
                return ChosenDownloader::Unavailable;
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
                tracing::error!(id = %row.id, "Transmission 配置不完整，拒绝内存假下载");
                return ChosenDownloader::Unavailable;
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
        _ => {
            tracing::error!(kind = %row.kind, "未知 Downloader 类型，拒绝内存假下载");
            ChosenDownloader::Unavailable
        }
    }
}

fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
