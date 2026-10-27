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
pub const LIBRARY_USER_VERSION: i64 = 5;
pub const SUBSCRIBE_USER_VERSION: i64 = 2;

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
            return Err(layout_error(
                name,
                format!("{name} database schema is missing required table {tbl}"),
            ));
        }
    }
    for &idx in indexes {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
            [idx],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(layout_error(
                name,
                format!("{name} database schema is missing required index {idx}"),
            ));
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
        return Err(layout_error(
            name,
            format!(
                "{name} database layout mismatch: expected application_id {expected_app_id}, user_version {expected_user_ver}, found application_id {app_id}, user_version {user_ver}"
            ),
        ));
    }
    match name {
        "app" => {
            validate_schema_objects(
                name,
                conn,
                crate::schema::v1::APP_TABLES,
                crate::schema::v1::APP_INDEXES,
            )?;
            validate_app_tables(conn)?;
        }
        "library" => {
            validate_schema_objects(
                name,
                conn,
                crate::schema::v1::LIBRARY_TABLES,
                crate::schema::v1::LIBRARY_INDEXES,
            )?;
            validate_library_objects(conn)?;
        }
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

/// A layout rejection is typed: an existing file written by another layout is
/// never migrated, adopted or reset, so callers (and tests) can tell it apart
/// from an ordinary bad argument.
///
/// Library and Subscribe layout refusals carry two runnable exits: rebuild the
/// old file (the supported path) or start on a fresh data directory. Both
/// preserve the original bytes.
fn layout_error(name: &str, message: String) -> StoreError {
    let expected_version = match name {
        "library" => LIBRARY_USER_VERSION,
        "subscribe" => SUBSCRIBE_USER_VERSION,
        _ => return StoreError::Invalid(message),
    };
    StoreError::LayoutMismatch(format!(
        "{message}. {name}.db requires user_version {expected_version}; \
stop the service and rebuild the old file with \
`bash scripts/storage-cutover-rebuild.sh --data-dir <data-dir>` \
(it backs the old file up and imports no old data; see docs/deploy.md), \
or preserve the original data directory and its consistent backup and start with a separate new directory using \
`CRAWLER_MEDIA_DATA=/absolute/path/to/new-data cargo run -p api --bin crawler-media -- serve` \
(see docs/deploy.md). No automatic reset or import is available"
    ))
}

/// The declared object lists are exhaustive, so a table or index the DDL does not
/// create means this file is not the layout this build serves — even though every
/// *required* object is still present (a stray `CREATE TABLE zz_extra` or
/// `CREATE INDEX zz_extra` satisfies `validate_schema_objects`). SQLite's own
/// objects (`sqlite_autoindex_*`, `sqlite_stat1` written by ANALYZE) start with
/// `sqlite_` and are not ours to declare, so they are skipped.
///
/// Two deliberate scopes:
///
/// - **Views and triggers are not part of the declared layout.** A fixture installs a
///   trigger to simulate a failure mid-window
///   (`tests/storage_work_bounds/scan_windows.rs`), so a file that carries one is still
///   a file this build serves. Only tables and indexes are checked here.
/// - **Indexes are now checked in both directions.** This arm used to be one-sided
///   because `LIBRARY_INDEXES` was a strict subset of the DDL (`files_kind_path` and
///   `terms_value` were missing), so an extra-object comparison would have refused a
///   healthy file. Both are listed now and the two lists are equal by count, which makes
///   the second direction usable — and it is the direction that matters for this branch:
///   a leftover index is write cost on every insert that nothing reads, i.e. exactly the
///   failure mode the v4/v5 rounds exist to remove. A file carrying one was not built by
///   this DDL, so it is refused rather than "opened and mostly works".
fn validate_library_objects(conn: &Connection) -> Result<(), StoreError> {
    let mut statement = conn.prepare(
        "SELECT type, name FROM sqlite_schema
          WHERE name NOT LIKE 'sqlite_%' AND type IN ('table', 'index')
          ORDER BY type, name",
    )?;
    let mut undeclared = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (kind, name) = row?;
        let declared = match kind.as_str() {
            "table" => crate::schema::v1::LIBRARY_TABLES.contains(&name.as_str()),
            "index" => crate::schema::v1::LIBRARY_INDEXES.contains(&name.as_str()),
            _ => true,
        };
        if !declared {
            undeclared.push(format!("{kind} {name}"));
        }
    }
    if undeclared.is_empty() {
        return Ok(());
    }
    Err(layout_error(
        "library",
        format!(
            "library database holds objects this layout does not declare: {}",
            undeclared.join(", ")
        ),
    ))
}

/// Retired app tables belong to another layout; never silently adopt them.
fn validate_app_tables(conn: &Connection) -> Result<(), StoreError> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )?;
    for table in statement.query_map([], |row| row.get::<_, String>(0))? {
        let table = table?;
        if !crate::schema::v1::APP_TABLES.contains(&table.as_str()) {
            return Err(StoreError::Invalid(format!(
                "app database has unsupported table {table}"
            )));
        }
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
