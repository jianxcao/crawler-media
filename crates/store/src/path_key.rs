//! Canonical `path_key` (storage review R1/R2, no backward compatibility).
//!
//! `path_key` is the identity used for uniqueness and lookup and is computed **once
//! at write time**; `display_path` keeps the caller's spelling and is never used to
//! find a row. SQL lookups normalize the probe and rewrite registered root aliases
//! without filesystem IO. Unregistered caller spellings must be resolved before
//! taking a shared Store lock; indexed probes use the recorded key.
//!
//! Policy:
//! - absolute paths only; repeated `/` and `.` segments are collapsed, a trailing
//!   `/` is dropped;
//! - `..` is **rejected** (kept verbatim, so the schema CHECK refuses the write and
//!   writers return `Invalid`): resolving it lexically is wrong across symlinks;
//! - Unicode is normalized to NFC, so macOS-style NFD names and NFC names are one key;
//! - a Library root is keyed by `fs::canonicalize` (symlinks and `..` resolved) when
//!   it exists, resolving existing ancestors for missing descendants; files use the canonical
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
    let aliases = table_for(conn)
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Arc::new(RwLock::new(aliases))
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
    let mut aliases = table_for(conn)
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if aliases.is_empty()
        && let Ok(mut stmt) = conn.prepare("SELECT display_path, path_key FROM library_roots")
        && let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
    {
        for row in rows.flatten() {
            let display = normalize_path_key(&row.0);
            if display != row.1 {
                aliases.push((display, row.1));
            }
        }
    }
    aliases.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    canonical_key(raw, &Arc::new(RwLock::new(aliases)))
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
/// directory exists; missing descendants retain the resolved ancestor identity.
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
            // Resolve the existing ancestors too. A not-yet-created root below
            // a symlink must keep this identity once its directory appears.
            crate::library_paths::resolved_path(path)?
                .to_string_lossy()
                .into_owned()
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
        // Registration already resolved each key. Reloading facts must not
        // reinterpret a later symlink, or observe slow mounts under Store.
        .map(|(display, key)| (normalize_path_key(&display), key))
        .filter(|(alias, key)| alias != key)
        .collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    *aliases.write().unwrap_or_else(|e| e.into_inner()) = rows;
    Ok(())
}

/// 前缀范围查询的上界：所有以 `prefix` 开头的键都 `< 返回值`。
///
/// 键列按 BINARY 排序（UTF-8 字节序 = 码点序），因此
/// `key >= prefix AND key < prefix_upper_bound(prefix)` 与
/// `key LIKE prefix || '%'` 选出同一批行，区别只在能不能吃索引做范围扫描：
/// `LIKE` 用不上索引，root 改指时「每条目录扫一遍整个 inputs 集」正是它造成的
/// （P7）。另外 `LIKE` 把 `%`/`_` 当通配符，路径里真的含这两个字符时会多匹配
/// 到并不在目录下的行——范围谓词没有这个问题。
///
/// 进位规则：从尾部找最后一个还能加一的字符，加一后截断（`/a/b` → `/a/c`）。
/// 尾部是 `char::MAX` 时继续向前借位；整串都是 `char::MAX` 时退化为追加哨兵，
/// 宁可多扫一点也不能漏。
pub fn prefix_upper_bound(prefix: &str) -> String {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        // 代理区不是合法 char，跳过它继续进位。
        let mut code = last as u32 + 1;
        if (0xD800..=0xDFFF).contains(&code) {
            code = 0xE000;
        }
        if let Some(next) = char::from_u32(code) {
            chars.push(next);
            return chars.into_iter().collect();
        }
    }
    format!("{prefix}\u{10FFFF}")
}

#[cfg(test)]
mod tests {
    use super::{normalize_path_key as n, prefix_upper_bound};

    /// 前缀下的每个键都落在 `[prefix, bound)` 里，而差一个字符的兄弟目录不落进来。
    #[test]
    fn prefix_upper_bound_covers_exactly_the_prefixed_keys() {
        for prefix in ["/a/b/", "/m_v/", "/100%/", "/媒体/", "/a b/", "/"] {
            let bound = prefix_upper_bound(prefix);
            assert!(prefix < bound.as_str(), "{prefix:?} 的上界必须大于前缀本身");
            for child in ["x.mkv", "子目录/y.nfo", "%", "_", "0", "zzz", ""] {
                let key = format!("{prefix}{child}");
                assert!(
                    key.as_str() < bound.as_str(),
                    "{key:?} 应小于上界 {bound:?}"
                );
            }
            // 兄弟目录（前缀去掉末尾 `/` 后换一个字符再挂 `/`）必须 ≥ 上界。
            let base = prefix.strip_suffix('/').unwrap_or(prefix);
            let sibling = format!("{base}X/");
            assert!(
                sibling.as_str() >= bound.as_str(),
                "兄弟目录 {sibling:?} 不该落进 {prefix:?} 的范围"
            );
        }
    }

    #[test]
    fn prefix_upper_bound_increments_the_last_incrementable_char() {
        assert_eq!(prefix_upper_bound("/a/b/"), "/a/b0");
        assert_eq!(prefix_upper_bound("/a/bz"), "/a/b{");
        assert_eq!(prefix_upper_bound("/媒体/"), "/媒体0");
        // 全是 char::MAX：借不出位，退回追加哨兵（宁可多扫也不漏）。
        let max = char::MAX.to_string();
        assert_eq!(prefix_upper_bound(&max), format!("{max}{max}"));
    }

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
