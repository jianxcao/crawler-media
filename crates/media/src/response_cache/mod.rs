use std::path::Path;
use rusqlite::Connection;

pub mod queries;
pub mod source;

pub use queries::{CacheRow, delete, details_for_prefixes, get, list, put};
pub use source::{CACHE_SOURCES, CacheSource};

pub const CACHE_APPLICATION_ID: u32 = 1129139251;
pub const CACHE_USER_VERSION: u32 = 1;

pub const CACHE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS tmdb_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_tmdb_cache_fetched
ON tmdb_cache(fetched_at DESC, entry_id ASC);

CREATE TABLE IF NOT EXISTS douban_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_douban_cache_fetched
ON douban_cache(fetched_at DESC, entry_id ASC);

CREATE TABLE IF NOT EXISTS tvdb_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_tvdb_cache_fetched
ON tvdb_cache(fetched_at DESC, entry_id ASC);

CREATE TABLE IF NOT EXISTS bangumi_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_bangumi_cache_fetched
ON bangumi_cache(fetched_at DESC, entry_id ASC);

CREATE TABLE IF NOT EXISTS anilist_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_anilist_cache_fetched
ON anilist_cache(fetched_at DESC, entry_id ASC);

CREATE TABLE IF NOT EXISTS fanart_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_fanart_cache_fetched
ON fanart_cache(fetched_at DESC, entry_id ASC);
"#;

#[derive(Debug, thiserror::Error)]
pub enum CacheDbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unknown cache source: {0}")]
    UnknownSource(String),
    #[error("unsupported cache schema version: found {found}, supported {supported}")]
    UnsupportedVersion { found: i64, supported: i64 },
    #[error("legacy format error: {0}")]
    LegacyFormat(String),
}

pub fn open(path: impl AsRef<Path>) -> Result<Connection, CacheDbError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    apply_connection_policy(&conn)?;
    initialize(&conn)?;
    Ok(conn)
}

pub fn initialize(conn: &Connection) -> Result<(), CacheDbError> {
    let current_app_id: u32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let current_user_version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current_app_id != 0 && current_app_id != CACHE_APPLICATION_ID {
        return Err(CacheDbError::LegacyFormat(format!(
            "mismatched cache application_id: expected {CACHE_APPLICATION_ID}, got {current_app_id}"
        )));
    }
    if current_user_version != 0 && current_user_version != CACHE_USER_VERSION {
        return Err(CacheDbError::UnsupportedVersion {
            found: current_user_version as i64,
            supported: CACHE_USER_VERSION as i64,
        });
    }

    conn.execute_batch(CACHE_SCHEMA_SQL)?;
    conn.execute_batch(&format!(
        "PRAGMA application_id = {CACHE_APPLICATION_ID};
         PRAGMA user_version = {CACHE_USER_VERSION};"
    ))?;
    Ok(())
}

fn apply_connection_policy(conn: &Connection) -> Result<(), CacheDbError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}
