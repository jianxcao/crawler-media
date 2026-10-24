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

/// Fixed alias snapshot for one observation plan. IO stays outside the Store lock;
/// every sidecar key in that plan uses this snapshot, not a later reload.
pub(crate) fn alias_snapshot(conn: &Connection) -> RootAliases {
    table_for(conn)
}

/// One alias table per library.db file, shared by every connection to it (several
/// `Store`s may open the same data dir). Keyed by the connection's database path; an
/// in-memory database has no path and is keyed by its own connection handle, so two
/// in-memory Stores never share aliases. Tables are only rebuilt from committed rows
/// (writers call [`reload_root_aliases`] after `commit`).
static TABLES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, RootAliases>>,
> = std::sync::LazyLock::new(Default::default);

fn table_key(conn: &Connection) -> Option<String> {
    conn.path()
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

fn memory_key(conn: &Connection) -> String {
    // SAFETY: the raw handle is only used as an identity, never dereferenced.
    format!("memory:{:p}", unsafe { conn.handle() })
}

pub(crate) fn table_for(conn: &Connection) -> RootAliases {
    let key = table_key(conn).unwrap_or_else(|| memory_key(conn));
    TABLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_default()
        .clone()
}

/// A fresh table for a new in-memory connection: a reused handle address must not
/// inherit a closed database's aliases.
fn fresh_table(conn: &Connection) -> RootAliases {
    if table_key(conn).is_some() {
        return table_for(conn);
    }
    let table = RootAliases::default();
    TABLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(memory_key(conn), table.clone());
    table
}

/// Lexical + NFC form, then the platform realpath when the path exists.
///
/// Existing paths go through `fs::canonicalize` so `/var` and `/private/var` (and any
/// other symlink spelling of the same file) share one key without waiting for a root
/// alias reload. Missing paths stay lexical: a deleted sidecar must still name the row
/// it is releasing. `..` is never resolved lexically.
pub fn canonical_existing_key(raw: &str) -> String {
    let lexical = normalize_path_key(raw);
    let resolved = match std::fs::canonicalize(&lexical) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => return lexical,
    };
    // macOS realpath returns NFD. NFC is applied again so Café and Cafe+combining
    // accent stay one key after the symlink rewrite.
    normalize_path_key(&resolved)
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

/// Canonical form of one path against the aliases committed on `conn`.
///
/// Display spelling stays with the caller for IO. Persistence, deletion and reduction
/// must all use this key so a symlink root and its real path name the same sidecar.
/// Identity of a path that may already be gone. Uses the committed root aliases and
/// does not touch the filesystem, so a missing file keeps the key it was stored with.
pub(crate) fn canonical_recorded_key(conn: &Connection, raw: &str) -> String {
    let mut aliases = table_for(conn).read().unwrap_or_else(|e| e.into_inner()).clone();
    if aliases.is_empty() {
        if let Ok(mut stmt) = conn.prepare("SELECT display_path, path_key FROM library_roots") {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    let display = normalize_path_key(&row.0);
                    if display != row.1 {
                        aliases.push((display, row.1));
                    }
                }
            }
        }
    }
    aliases.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    canonical_key(raw, &Arc::new(RwLock::new(aliases)))
}

pub(crate) fn canonical_path_key(conn: &Connection, raw: &str) -> String {
    canonical_key(raw, &table_for(conn))
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
    let aliases = fresh_table(conn);
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

/// Rebuild the alias table from committed `library_roots` (small table). Writers call it
/// only after their transaction committed, so a rolled-back root is never published.
pub(crate) fn reload_root_aliases(conn: &Connection) -> Result<(), StoreError> {
    let aliases = table_for(conn);
    let mut stmt = conn.prepare("SELECT display_path, path_key FROM library_roots")?;
    let mut rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flat_map(|(display, key)| {
            let display_key = normalize_path_key(&display);
            // The root key is `fs::canonicalize` at registration. A later lookup of the
            // same directory can still spell a platform alias (`/var` vs `/private/var`)
            // that is not the registered display path. Both spellings name `key`.
            let resolved = std::fs::canonicalize(&key)
                .ok()
                .map(|path| normalize_path_key(&path.to_string_lossy()))
                .filter(|resolved| resolved != &key);
            // The stored key can be a platform alias of the real directory. Always
            // point the shorter spelling at the realpath, never the other way around.
            let (alias, canonical) = match &resolved {
                Some(real) if key == *real => (display_key.clone(), key.clone()),
                Some(real) => (key.clone(), real.clone()),
                None => (display_key.clone(), key.clone()),
            };
            [
                Some((display_key, canonical.clone())),
                Some((alias, canonical)),
            ]
        })
        .flatten()
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

#[cfg(test)]
mod alias_isolation {
    use rusqlite::Connection;

    fn conn_with_alias(display: &str, key: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        super::register(&conn).unwrap();
        conn.execute_batch("CREATE TABLE library_roots (display_path TEXT, path_key TEXT)")
            .unwrap();
        conn.execute("INSERT INTO library_roots VALUES (?1, ?2)", [display, key])
            .unwrap();
        super::reload_root_aliases(&conn).unwrap();
        conn
    }

    fn key(conn: &Connection, raw: &str) -> String {
        conn.query_row("SELECT norm_path_key(?1)", [raw], |r| r.get(0))
            .unwrap()
    }

    /// C09: two in-memory databases never share one alias table.
    #[test]
    fn in_memory_connections_keep_their_own_aliases() {
        let a = conn_with_alias("/media/alias", "/real/a");
        let b = conn_with_alias("/media/alias", "/real/b");
        assert_eq!(key(&a, "/media/alias/f.mkv"), "/real/a/f.mkv");
        assert_eq!(key(&b, "/media/alias/f.mkv"), "/real/b/f.mkv");
    }

    /// C09: a rolled-back root never reaches the alias table; publication happens
    /// only from committed rows.
    #[test]
    fn rolled_back_root_is_not_published() {
        let conn = conn_with_alias("/media/alias", "/real/a");
        {
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute(
                "INSERT INTO library_roots VALUES ('/media/other', '/real/other')",
                [],
            )
            .unwrap();
        }
        super::reload_root_aliases(&conn).unwrap();
        assert_eq!(key(&conn, "/media/other/f.mkv"), "/media/other/f.mkv");
    }
}
