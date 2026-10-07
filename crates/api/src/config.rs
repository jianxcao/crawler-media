//! Centralized environment variable registration.
//!
//! All `CRAWLER_MEDIA_*` variables are parsed here at startup.
//! Each field documents its purpose and default. Scattered `std::env::var`
//! calls elsewhere should migrate here over time.

use std::path::PathBuf;

/// All environment-sourced configuration for the server process.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Data directory for SQLite, cache, images, etc.
    /// Env: `CRAWLER_MEDIA_DATA`, default: `"data"`.
    pub data_dir: PathBuf,

    /// Shared authentication token (required for serve mode).
    /// Env: `CRAWLER_MEDIA_TOKEN`.
    pub token: Option<String>,

    /// Initial Administrator password (independent from CLI bearer token).
    /// Env: `CRAWLER_MEDIA_ADMIN_PASSWORD` (or `_FILE`).
    pub admin_password: Option<String>,

    /// Listen address.
    /// Env: `CRAWLER_MEDIA_LISTEN`, default: `"127.0.0.1:18765"`.
    pub listen: String,

    /// Static UI directory (production: `/usr/share/crawler-media/ui`).
    /// Env: `CRAWLER_MEDIA_UI`.
    pub ui_dir: Option<PathBuf>,

    /// CORS allowed origins (comma-separated).
    /// Env: `CRAWLER_MEDIA_CORS_ORIGINS`.
    pub cors_origins: Vec<String>,

    /// TMDB API key (seeded into settings KV on startup).
    /// Env: `CRAWLER_MEDIA_TMDB_KEY`.
    pub tmdb_key: Option<String>,

    /// TVDB API key.
    /// Env: `CRAWLER_MEDIA_TVDB_KEY`.
    pub tvdb_key: Option<String>,

    /// Enable managed Chromium browser.
    /// Env: `CRAWLER_MEDIA_BROWSER` (flag: `1`/`true`/`yes`).
    pub browser_enabled: bool,

    // -- qBittorrent --
    /// Env: `CRAWLER_MEDIA_QB_URL`.
    pub qb_url: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_USER`.
    pub qb_user: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_PASS`.
    pub qb_pass: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_CATEGORY`.
    pub qb_category: Option<String>,
    /// Path mapping for qBittorrent (container → host).
    /// Env: `CRAWLER_MEDIA_QB_PATH_MAP`.
    pub qb_path_map_raw: Option<String>,

    // -- Transmission --
    /// Path mapping for Transmission.
    /// Env: `CRAWLER_MEDIA_TR_PATH_MAP`.
    pub tr_path_map_raw: Option<String>,

    /// Metadata proxy URL (http://... or socks5://...).
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY`.
    pub metadata_proxy: Option<String>,
    /// Metadata proxy username.
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY_USER`.
    pub metadata_proxy_user: Option<String>,
    /// Metadata proxy password.
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY_PASS` (or `_FILE`).
    pub metadata_proxy_pass: Option<String>,
}

impl ServerConfig {
    /// Parse all `CRAWLER_MEDIA_*` environment variables at startup.
    ///
    /// Unknown `CRAWLER_MEDIA_*` variables are logged as warnings to catch
    /// typos (e.g. `CRAWLER_MEDIA_TMDB_KET` instead of `_KEY`).
    pub fn from_env() -> Self {
        let known = [
            "CRAWLER_MEDIA_DATA",
            "CRAWLER_MEDIA_TOKEN",
            "CRAWLER_MEDIA_ADMIN_PASSWORD",
            "CRAWLER_MEDIA_LISTEN",
            "CRAWLER_MEDIA_UI",
            "CRAWLER_MEDIA_CORS_ORIGINS",
            "CRAWLER_MEDIA_TMDB_KEY",
            "CRAWLER_MEDIA_TVDB_KEY",
            "CRAWLER_MEDIA_BROWSER",
            "CRAWLER_MEDIA_QB_URL",
            "CRAWLER_MEDIA_QB_USER",
            "CRAWLER_MEDIA_QB_PASS",
            "CRAWLER_MEDIA_QB_CATEGORY",
            "CRAWLER_MEDIA_QB_PATH_MAP",
            "CRAWLER_MEDIA_TR_PATH_MAP",
            "CRAWLER_MEDIA_METADATA_PROXY",
            "CRAWLER_MEDIA_METADATA_PROXY_USER",
            "CRAWLER_MEDIA_METADATA_PROXY_PASS",
            "CRAWLER_MEDIA_URL", // CLI-only
            // Secret-file variants (Docker/K8s secret volumes).
            "CRAWLER_MEDIA_TOKEN_FILE",
            "CRAWLER_MEDIA_ADMIN_PASSWORD_FILE",
            "CRAWLER_MEDIA_TMDB_KEY_FILE",
            "CRAWLER_MEDIA_TVDB_KEY_FILE",
            "CRAWLER_MEDIA_QB_PASS_FILE",
            "CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE",
        ];

        // Warn about unrecognized CRAWLER_MEDIA_* variables.
        for (key, _) in std::env::vars() {
            if key.starts_with("CRAWLER_MEDIA_") && !known.contains(&key.as_str()) {
                eprintln!("[config] warning: unrecognized environment variable {key}");
            }
        }

        Self {
            data_dir: data_dir_from_env(),
            token: secret("CRAWLER_MEDIA_TOKEN"),
            admin_password: secret("CRAWLER_MEDIA_ADMIN_PASSWORD"),
            listen: nonempty("CRAWLER_MEDIA_LISTEN").unwrap_or_else(|| "127.0.0.1:18765".into()),
            ui_dir: nonempty("CRAWLER_MEDIA_UI").map(PathBuf::from),
            cors_origins: cors_origins_from_env(),
            tmdb_key: secret("CRAWLER_MEDIA_TMDB_KEY"),
            tvdb_key: secret("CRAWLER_MEDIA_TVDB_KEY"),
            browser_enabled: env_flag("CRAWLER_MEDIA_BROWSER"),
            qb_url: nonempty("CRAWLER_MEDIA_QB_URL"),
            qb_user: nonempty("CRAWLER_MEDIA_QB_USER"),
            qb_pass: secret("CRAWLER_MEDIA_QB_PASS"),
            qb_category: nonempty("CRAWLER_MEDIA_QB_CATEGORY"),
            qb_path_map_raw: nonempty("CRAWLER_MEDIA_QB_PATH_MAP"),
            tr_path_map_raw: nonempty("CRAWLER_MEDIA_TR_PATH_MAP"),
            metadata_proxy: nonempty("CRAWLER_MEDIA_METADATA_PROXY"),
            metadata_proxy_user: nonempty("CRAWLER_MEDIA_METADATA_PROXY_USER"),
            metadata_proxy_pass: secret("CRAWLER_MEDIA_METADATA_PROXY_PASS"),
        }
    }
}

/// Resolve the data directory from `CRAWLER_MEDIA_DATA` (default `"data"`).
///
/// Shared by [`ServerConfig`] and by modules that need the path without a full
/// config instance, so the parsing rule lives in exactly one place.
pub fn data_dir_from_env() -> PathBuf {
    PathBuf::from(nonempty("CRAWLER_MEDIA_DATA").unwrap_or_else(|| "data".into()))
}

/// Parse the CORS allow-list from `CRAWLER_MEDIA_CORS_ORIGINS` (comma separated).
pub fn cors_origins_from_env() -> Vec<String> {
    nonempty("CRAWLER_MEDIA_CORS_ORIGINS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default()
}

/// True when `CRAWLER_MEDIA_TMDB_KEY` (or its `_FILE` variant) supplies a key,
/// i.e. the value is pinned by the environment rather than the settings UI.
pub fn tmdb_key_from_env_is_set() -> bool {
    secret("CRAWLER_MEDIA_TMDB_KEY").is_some()
}

fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Read a secret from env var `name`, falling back to the file path given by
/// `{name}_FILE`. Docker Swarm / Kubernetes secret volumes use this pattern so
/// credentials never appear in `docker inspect` or `/proc/<pid>/environ`.
fn secret(name: &str) -> Option<String> {
    if let Some(value) = nonempty(name) {
        return Some(value);
    }
    let file_var = format!("{name}_FILE");
    let path = nonempty(&file_var)?;
    read_secret_file(&path, &file_var)
}

/// Read and trim a secret-file's contents. Empty files yield `None` so a
/// mounted-but-empty secret does not silently override a real value.
fn read_secret_file(path: &str, var_name: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(err) => {
            eprintln!("[config] warning: could not read {var_name}={path}: {err}");
            None
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_file_returns_trimmed_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  s3cret\n").unwrap();
        assert_eq!(
            read_secret_file(path.to_str().unwrap(), "X_FILE"),
            Some("s3cret".into())
        );
    }

    #[test]
    fn empty_secret_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "\n  \n").unwrap();
        assert_eq!(read_secret_file(path.to_str().unwrap(), "X_FILE"), None);
    }

    #[test]
    fn missing_secret_file_is_none() {
        assert_eq!(
            read_secret_file("/nonexistent/definitely-missing", "X_FILE"),
            None
        );
    }
}
