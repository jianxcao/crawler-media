use std::path::Path;
use rusqlite::{Connection, OpenFlags};

use crate::StoreError;

pub const APP_APPLICATION_ID: u32 = 1129139248;
pub const LIBRARY_APPLICATION_ID: u32 = 1129139249;
pub const SUBSCRIBE_APPLICATION_ID: u32 = 1129139250;
pub const CACHE_APPLICATION_ID: u32 = 1129139251;
pub const V1_USER_VERSION: i64 = 1;

pub const APP_DDL: &str = include_str!("v1/app.sql");
pub const LIBRARY_DDL: &str = include_str!("v1/library.sql");
pub const SUBSCRIBE_DDL: &str = include_str!("v1/subscribe.sql");
pub const CACHE_DDL: &str = include_str!("v1/cache.sql");

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
    let table_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(table_count == 0)
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
        return Err(StoreError::Invalid(format!(
            "{name} database layout mismatch: expected application_id {expected_app_id}, user_version {expected_user_ver}, found application_id {app_id}, user_version {user_ver}"
        )));
    }
    Ok(())
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
    validate_database_layout(name, &conn, expected_app_id, expected_user_ver)
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
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(ddl)?;
        tx.pragma_update(None, "application_id", expected_app_id)?;
        tx.pragma_update(None, "user_version", expected_user_ver)?;
        tx.commit()?;
    } else {
        validate_database_layout(name, conn, expected_app_id, expected_user_ver)?;
    }
    Ok(())
}
