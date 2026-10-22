//! Stable `path_key` normalization (risk 1 of the 2026-10-22 storage review).
//!
//! `display_path` keeps the caller's spelling; `path_key` is the identity used for
//! uniqueness and lookup. Normalization is purely lexical and filesystem-equivalent
//! on every platform we run on: repeated separators collapse, `.` components drop and a
//! trailing separator is removed. `..` is kept verbatim (resolving it lexically is not
//! equivalent across symlinks), and no case or Unicode folding is applied, so a key is
//! still a usable path for the same bytes.
use rusqlite::Connection;

use crate::StoreError;

/// SQL name of the registered scalar function: `norm_path_key(text)`.
pub(crate) const SQL_FN: &str = "norm_path_key";

pub fn normalize_path_key(raw: &str) -> String {
    let absolute = raw.starts_with('/');
    let parts: Vec<&str> = raw
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

pub(crate) fn register(conn: &Connection) -> Result<(), StoreError> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        SQL_FN,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(match ctx.get_raw(0) {
                rusqlite::types::ValueRef::Text(bytes) => {
                    let text = String::from_utf8_lossy(bytes);
                    rusqlite::types::Value::Text(normalize_path_key(&text))
                }
                rusqlite::types::ValueRef::Null => rusqlite::types::Value::Null,
                other => rusqlite::types::Value::from(other),
            })
        },
    )?;
    Ok(())
}

/// Rewrites legacy verbatim keys to their normalized form, one row at a time, in one
/// transaction. A row whose normalized key already belongs to another row is left
/// untouched (and logged) instead of failing open or merging data.
pub(crate) fn migrate_library_keys(conn: &Connection) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    let mut migrated = 0usize;
    for (table, id_col) in [("library_roots", "root_id"), ("library_files", "file_id")] {
        let pending: Vec<(String, String)> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {id_col}, path_key FROM {table} WHERE path_key <> {SQL_FN}(path_key)"
            ))?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        for (id, key) in pending {
            let target = normalize_path_key(&key);
            let taken: bool = tx.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE path_key = ?1)"),
                [&target],
                |r| r.get(0),
            )?;
            if taken {
                tracing::warn!(table, id = %id, legacy = %key, normalized = %target,
                    "path_key migration skipped: normalized key already recorded by another row");
                continue;
            }
            tx.execute(
                &format!("UPDATE {table} SET path_key = ?2 WHERE {id_col} = ?1"),
                [&id, &target],
            )?;
            migrated += 1;
        }
    }
    migrated += tx.execute(
        &format!(
            "UPDATE library_metadata_inputs SET path_key = {SQL_FN}(path_key)
             WHERE path_key <> {SQL_FN}(path_key)"
        ),
        [],
    )?;
    tx.commit()?;
    if migrated > 0 {
        tracing::info!(migrated, "normalized legacy path_key rows");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_path_key as n;

    #[test]
    fn lexical_and_filesystem_equivalent() {
        assert_eq!(n("/a//b/./c/"), "/a/b/c");
        assert_eq!(n("/"), "/");
        assert_eq!(n("/a/../b"), "/a/../b");
        assert_eq!(n("rel/./x"), "rel/x");
        assert_eq!(n("Movie É.mkv"), "Movie É.mkv");
    }
}
