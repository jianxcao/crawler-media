pub use media::response_cache::{
    CACHE_APPLICATION_ID, CACHE_SCHEMA_SQL as CACHE_DDL, CACHE_USER_VERSION,
};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

use crate::StoreError;

pub const APP_APPLICATION_ID: u32 = 1129139248;
pub const LIBRARY_APPLICATION_ID: u32 = 1129139249;
pub const SUBSCRIBE_APPLICATION_ID: u32 = 1129139250;
pub const V1_USER_VERSION: i64 = 1;
pub const LIBRARY_USER_VERSION: i64 = 1;
pub const SUBSCRIBE_USER_VERSION: i64 = 1;

pub const APP_DDL: &str = include_str!("v1/app.sql");
pub const LIBRARY_DDL: &str = include_str!("v1/library.sql");
pub const SUBSCRIBE_DDL: &str = include_str!("v1/subscribe.sql");
pub const CACHE_USER_VERSION_I64: i64 = CACHE_USER_VERSION as i64;

pub fn read_application_id(conn: &Connection) -> Result<u32, StoreError> {
    let val: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    Ok(val as u32)
}

pub fn read_user_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

pub fn is_uninitialized(conn: &Connection) -> Result<bool, StoreError> {
    let app_id = read_application_id(conn)?;
    let user_ver = read_user_version(conn)?;
    if app_id != 0 || user_ver != 0 {
        return Ok(false);
    }
    let object_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(object_count == 0)
}

pub fn validate_database_layout(
    name: &str,
    conn: &Connection,
    expected_app_id: u32,
    expected_user_ver: i64,
) -> Result<(), StoreError> {
    let app_id = read_application_id(conn)?;
    let user_ver = read_user_version(conn)?;
    if app_id != expected_app_id || user_ver != expected_user_ver {
        return Err(layout_error(
            name,
            format!(
                "{name} database layout mismatch: expected application_id {expected_app_id}, user_version {expected_user_ver}, found application_id {app_id}, user_version {user_ver}"
            ),
        ));
    }
    super::definitions::validate(name, conn)?;
    Ok(())
}

/// Invalid files never get adopted, rewritten or repaired during normal open.
pub(super) fn layout_error(name: &str, message: String) -> StoreError {
    let expected_version = match name {
        "library" => LIBRARY_USER_VERSION,
        "subscribe" => SUBSCRIBE_USER_VERSION,
        _ => return StoreError::Invalid(message),
    };
    StoreError::LayoutMismatch(format!(
        "{message}. {name}.db must match the sole current definition (user_version {expected_version}); \
         preserve the data directory and its consistent backup. Use a separate empty directory with \
         `CRAWLER_MEDIA_DATA=/absolute/path/to/new-data cargo run -p api --bin crawler-media -- serve`. \
         Startup never imports data or changes an incompatible definition (see docs/deploy.md)"
    ))
}

/// Validate the sole source-response cache definition.
pub fn validate_cache_layout(conn: &Connection) -> Result<(), StoreError> {
    media::response_cache::validate_schema(conn).map_err(|error| {
        StoreError::Invalid(format!(
            "cache database schema mismatch: {error}. cache.db must match the sole current definition; \
             startup does not rebuild or import data (see docs/deploy.md)"
        ))
    })
}

pub fn preflight_check_existing_file(
    name: &str,
    path: &Path,
    expected_app_id: u32,
    expected_user_ver: i64,
) -> Result<(), StoreError> {
    if !path.exists() {
        return Ok(());
    }
    // Check if 0 bytes
    let metadata = std::fs::metadata(path)?;
    if metadata.len() == 0 {
        return Ok(());
    }

    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if is_uninitialized(&conn)? {
        return Ok(());
    }
    validate_database_layout(name, &conn, expected_app_id, expected_user_ver).map_err(|error| {
        tracing::error!(database = name, path = %path.display(), error = %error, "database layout rejected");
        error
    })?;
    if name == "cache" {
        validate_cache_layout(&conn)?;
    }
    Ok(())
}

pub fn initialize_or_validate(
    name: &str,
    conn: &Connection,
    ddl: &str,
    expected_app_id: u32,
    expected_user_ver: i64,
) -> Result<(), StoreError> {
    if is_uninitialized(conn)? {
        conn.execute("PRAGMA foreign_keys = ON;", [])?;
        let tx = crate::write_tx(conn)?;
        tx.execute_batch(ddl)?;
        tx.pragma_update(None, "application_id", expected_app_id)?;
        tx.pragma_update(None, "user_version", expected_user_ver)?;
        tx.commit()?;
    } else {
        validate_database_layout(name, conn, expected_app_id, expected_user_ver)?;
        if name == "cache" {
            validate_cache_layout(conn)?;
        }
    }
    Ok(())
}
