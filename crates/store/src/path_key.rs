//! Canonical `path_key` (storage review R1/R2, no backward compatibility).
//!
//! `path_key` is the identity used for uniqueness and lookup and is computed **once
//! at write time**; `display_path` keeps the caller's spelling and is never used to
//! find a row. Lookups canonicalize their probe once per statement (constant
//! argument of the deterministic `norm_path_key()` function, so SQLite evaluates it
//! once and still seeks the UNIQUE index).
//!
//! Policy:
//! - absolute paths only; repeated `/` and `.` segments are collapsed, a trailing
//!   `/` is dropped;
//! - `..` is **rejected** (kept verbatim, so the schema CHECK refuses the write and
//!   writers return `Invalid`): resolving it lexically is wrong across symlinks;
//! - Unicode is normalized to NFC, so macOS-style NFD names and NFC names are one key;
//! - a Library root is keyed by `fs::canonicalize` (symlinks and `..` resolved) when
//!   it exists; a file under any spelling of that root is keyed under the canonical
//!   root prefix via the per-Store root alias table;
//! - **no case folding**: the deployment target (Linux ext4/xfs/btrfs, Docker) is
//!   case-sensitive, and folding there would merge distinct files.
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use std::sync::RwLock;
use unicode_normalization::UnicodeNormalization;

use crate::StoreError;

/// `(alias prefix, canonical root key)`, longest alias first.
pub(crate) type RootAliases = Arc<RwLock<Vec<(String, String)>>>;

/// One alias table per library.db file, shared by every connection to it (several
/// `Store`s may open the same data dir). Keyed by the connection's database path.
static TABLES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, RootAliases>>,
> = std::sync::LazyLock::new(Default::default);

fn table_for(conn: &Connection) -> RootAliases {
    let key = conn.path().unwrap_or("").to_string();
    TABLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_default()
        .clone()
}

/// Lexical + NFC canonical form (no root alias rewrite).
pub fn normalize_path_key(raw: &str) -> String {
    let nfc: String = raw.nfc().collect();
    let absolute = nfc.starts_with('/');
    let parts: Vec<&str> = nfc
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

/// Canonical form with the root alias table applied.
pub(crate) fn canonical_key(raw: &str, aliases: &RootAliases) -> String {
    let key = normalize_path_key(raw);
    for (alias, root) in aliases.read().unwrap_or_else(|e| e.into_inner()).iter() {
        if key == *alias {
            return root.clone();
        }
        if let Some(rest) = key.strip_prefix(alias.as_str())
            && rest.starts_with('/')
        {
            return format!("{root}{rest}");
        }
    }
    key
}

/// Key for a Library root: absolute, no `..` in the request, symlinks resolved when the
/// directory exists (otherwise the lexical canonical form).
pub fn canonical_root_key(display: &str) -> Result<String, StoreError> {
    let path = Path::new(display);
    if !path.is_absolute() {
        return Err(StoreError::Invalid(format!(
            "root {display} must be absolute"
        )));
    }
    let resolved = match std::fs::canonicalize(path) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => {
            if path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(StoreError::Invalid(format!("root {display} contains `..`")));
            }
            display.to_string()
        }
    };
    Ok(normalize_path_key(&resolved))
}

pub(crate) fn register(conn: &Connection) -> Result<(), StoreError> {
    use rusqlite::functions::FunctionFlags;
    let aliases = table_for(conn);
    conn.create_scalar_function(
        "norm_path_key",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            Ok(match ctx.get_raw(0) {
                rusqlite::types::ValueRef::Text(bytes) => {
                    let text = String::from_utf8_lossy(bytes);
                    rusqlite::types::Value::Text(canonical_key(&text, &aliases))
                }
                rusqlite::types::ValueRef::Null => rusqlite::types::Value::Null,
                other => rusqlite::types::Value::from(other),
            })
        },
    )?;
    Ok(())
}

/// Rebuild the alias table from `library_roots` (small table; called after every root
/// write, on the writing connection so an uncommitted root is already visible).
pub(crate) fn reload_root_aliases(conn: &Connection) -> Result<(), StoreError> {
    let aliases = table_for(conn);
    let mut stmt = conn.prepare("SELECT display_path, path_key FROM library_roots")?;
    let mut rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(display, key)| (normalize_path_key(&display), key))
        .filter(|(alias, key)| alias != key)
        .collect();
    rows.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    *aliases.write().unwrap_or_else(|e| e.into_inner()) = rows;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_path_key as n;

    #[test]
    fn lexical_nfc_and_parent_kept_for_rejection() {
        assert_eq!(n("/a//b/./c/"), "/a/b/c");
        assert_eq!(n("/"), "/");
        assert_eq!(n("/a/../b"), "/a/../b");
        assert_eq!(n("/Cafe\u{301}.mkv"), "/Caf\u{e9}.mkv");
        assert_eq!(n("/Movie.MKV"), "/Movie.MKV", "no case folding");
    }
}
