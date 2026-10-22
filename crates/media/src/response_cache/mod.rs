use rusqlite::Connection;
use std::path::Path;

pub mod queries;
pub mod source;

pub use queries::{CacheRow, delete, details_for_prefixes, get, list, put};
pub use source::{CACHE_SOURCES, CacheSource};

pub const CACHE_APPLICATION_ID: u32 = 1129139251;
pub const CACHE_USER_VERSION: u32 = 1;

pub const CACHE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS tmdb_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_tmdb_cache_fetched ON tmdb_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_tmdb_cache_retention ON tmdb_cache(fetched_at ASC, entry_id ASC);

CREATE TABLE IF NOT EXISTS douban_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_douban_cache_fetched ON douban_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_douban_cache_retention ON douban_cache(fetched_at ASC, entry_id ASC);

CREATE TABLE IF NOT EXISTS tvdb_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_tvdb_cache_fetched ON tvdb_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_tvdb_cache_retention ON tvdb_cache(fetched_at ASC, entry_id ASC);

CREATE TABLE IF NOT EXISTS bangumi_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_bangumi_cache_fetched ON bangumi_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_bangumi_cache_retention ON bangumi_cache(fetched_at ASC, entry_id ASC);

CREATE TABLE IF NOT EXISTS anilist_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_anilist_cache_fetched ON anilist_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_anilist_cache_retention ON anilist_cache(fetched_at ASC, entry_id ASC);

CREATE TABLE IF NOT EXISTS fanart_cache (
    entry_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,
    body TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_fanart_cache_fetched ON fanart_cache(fetched_at DESC, entry_id ASC);
CREATE INDEX IF NOT EXISTS idx_fanart_cache_retention ON fanart_cache(fetched_at ASC, entry_id ASC);
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

    // Preflight check existing database read-only before modifying or setting WAL policy
    if path.exists() && std::fs::metadata(path)?.len() > 0 {
        let ro = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let app_id: u32 = ro.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let user_ver: u32 = ro.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let has_tables: bool = ro.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%')",
            [],
            |r| r.get(0),
        )?;

        if app_id == CACHE_APPLICATION_ID && user_ver == CACHE_USER_VERSION {
            validate_schema(&ro)?;
        } else if has_tables
            || (app_id != 0 && app_id != CACHE_APPLICATION_ID)
            || (user_ver != 0 && user_ver != CACHE_USER_VERSION)
        {
            return Err(CacheDbError::LegacyFormat(format!(
                "invalid cache layout at {}: app_id={}, user_version={}, has_schema_objects={}",
                path.display(),
                app_id,
                user_ver,
                has_tables
            )));
        }
    }

    let conn = Connection::open(path)?;
    apply_connection_policy(&conn)?;
    initialize(&conn)?;
    Ok(conn)
}

const CACHE_TABLES: [&str; 6] = [
    "tmdb_cache",
    "douban_cache",
    "tvdb_cache",
    "bangumi_cache",
    "anilist_cache",
    "fanart_cache",
];

const CACHE_INDEXES: [&str; 12] = [
    "idx_tmdb_cache_fetched",
    "idx_tmdb_cache_retention",
    "idx_douban_cache_fetched",
    "idx_douban_cache_retention",
    "idx_tvdb_cache_fetched",
    "idx_tvdb_cache_retention",
    "idx_bangumi_cache_fetched",
    "idx_bangumi_cache_retention",
    "idx_anilist_cache_fetched",
    "idx_anilist_cache_retention",
    "idx_fanart_cache_fetched",
    "idx_fanart_cache_retention",
];

/// Checks the required cache schema objects without changing the database.
pub fn validate_schema(conn: &Connection) -> Result<(), CacheDbError> {
    for (kind, name) in CACHE_TABLES
        .iter()
        .map(|name| ("table", *name))
        .chain(CACHE_INDEXES.iter().map(|name| ("index", *name)))
    {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2)",
            rusqlite::params![kind, name],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CacheDbError::LegacyFormat(format!(
                "cache schema is missing required {kind} {name}"
            )));
        }
    }
    Ok(())
}

pub fn initialize(conn: &Connection) -> Result<(), CacheDbError> {
    let current_app_id: u32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let current_user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current_app_id != 0 && current_app_id != CACHE_APPLICATION_ID {
        return Err(CacheDbError::LegacyFormat(format!(
            "mismatched cache application_id: expected {CACHE_APPLICATION_ID}, got {current_app_id}"
        )));
    }
    if current_user_version != 0 && current_user_version != CACHE_USER_VERSION as i64 {
        return Err(CacheDbError::UnsupportedVersion {
            found: current_user_version,
            supported: CACHE_USER_VERSION as i64,
        });
    }

    let has_objects: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%')",
        [],
        |r| r.get(0),
    )?;
    if current_app_id == CACHE_APPLICATION_ID && current_user_version == CACHE_USER_VERSION as i64 {
        validate_schema(conn)?;
        return Ok(());
    }
    if has_objects {
        return Err(CacheDbError::LegacyFormat(
            "unversioned cache database contains schema objects".into(),
        ));
    }

    // Apply DDL and identity atomically; only an empty, unversioned file is initialized.
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;\n{CACHE_SCHEMA_SQL}\nPRAGMA application_id = {CACHE_APPLICATION_ID};\nPRAGMA user_version = {CACHE_USER_VERSION};\nCOMMIT;"
    ))?;
    Ok(())
}

fn apply_connection_policy(conn: &Connection) -> Result<(), CacheDbError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}
