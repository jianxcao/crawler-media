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

pub const JOBS_APPLICATION_ID: u32 = 1129139252;
pub const JOBS_USER_VERSION: i64 = 1;

const JOBS_REPAIR_HINT: &str = "jobs.db is an active queue database containing durable job definitions and states, not a disposable cache. \
Do not delete or recreate automatically; please back up data/jobs.db before repairing or running migrations.";

fn jobs_layout_error(error: StoreError) -> StoreError {
    StoreError::Invalid(format!("{error}. {JOBS_REPAIR_HINT}"))
}

pub fn validate_jobs_layout(conn: &Connection) -> Result<(), StoreError> {
    let app_id = read_application_id(conn)?;
    let user_ver = read_user_version(conn)?;
    if app_id != JOBS_APPLICATION_ID || user_ver != JOBS_USER_VERSION {
        return Err(StoreError::Invalid(format!(
            "jobs database layout mismatch: expected application_id {JOBS_APPLICATION_ID}, user_version {JOBS_USER_VERSION}, found application_id {app_id}, user_version {user_ver}. {JOBS_REPAIR_HINT}"
        )));
    }
    validate_schema_objects(
        "jobs",
        conn,
        crate::schema::v1::JOBS_TABLES,
        crate::schema::v1::JOBS_INDEXES,
    )
    .map_err(jobs_layout_error)
}

pub fn initialize_jobs_layout(conn: &Connection) -> Result<(), StoreError> {
    if is_uninitialized(conn)? {
        conn.pragma_update(None, "application_id", JOBS_APPLICATION_ID)?;
        conn.pragma_update(None, "user_version", JOBS_USER_VERSION)?;
    } else {
        validate_jobs_layout(conn)?;
    }
    Ok(())
}

pub fn validate_schema_objects(
    name: &str,
    conn: &Connection,
    tables: &[&str],
    indexes: &[&str],
) -> Result<(), StoreError> {
    for &tbl in tables {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [tbl],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Invalid(format!(
                "{name} database schema is missing required table {tbl}"
            )));
        }
    }
    for &idx in indexes {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
            [idx],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Invalid(format!(
                "{name} database schema is missing required index {idx}"
            )));
        }
    }
    Ok(())
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
    match name {
        "app" => validate_schema_objects(
            name,
            conn,
            crate::schema::v1::APP_TABLES,
            crate::schema::v1::APP_INDEXES,
        )?,
        "library" => validate_schema_objects(
            name,
            conn,
            crate::schema::v1::LIBRARY_TABLES,
            crate::schema::v1::LIBRARY_INDEXES,
        )?,
        "subscribe" => validate_schema_objects(
            name,
            conn,
            crate::schema::v1::SUBSCRIBE_TABLES,
            crate::schema::v1::SUBSCRIBE_INDEXES,
        )?,
        _ => {}
    }
    Ok(())
}

/// 五文件干净切换后 `cache.db` 只承载外部响应体：它是派生数据、不是事实来源
/// （`docs/superpowers/specs/2026-10-18-source-separated-cache.md`）。
/// 切换前布局（`bd6c3c8` 之前的 `*_cache_recent_idx` 索引名）按计划被**拒绝**，
/// 不提供兼容视图、双读或旧数据导入；因此错误信息必须直接给出可执行的停机重建入口。
pub fn validate_cache_layout(conn: &Connection) -> Result<(), StoreError> {
    media::response_cache::validate_schema(conn).map_err(|error| {
        StoreError::Invalid(format!(
            "cache database schema mismatch: {error}. this cache.db predates the storage clean cutover; \
stop the service and rebuild the derived cache with \
`bash scripts/storage-cutover-rebuild.sh --data-dir <data-dir>` (it backs the old file up and imports no old data; see docs/deploy.md)"
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
    validate_database_layout(name, &conn, expected_app_id, expected_user_ver)?;
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
