use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};
use std::collections::{BTreeMap, HashSet};

mod status;
mod unidentified;
pub use status::RootReassignment;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: String,
    pub kind: MediaKind,
    pub path: PathBuf,
    /// Primary root of the kind's default Library (library `is_default` and
    /// `sort_order = 0`). Additional roots of that Library, and every root of a
    /// non-default Library, are reported as `false` (the directory surface's
    /// "extra roots").
    pub is_default: bool,
    /// Persisted owning Library; every current root has exactly one owner.
    pub library_id: String,
    /// Position within the owning library; 0 = primary root.
    pub sort_order: i32,
    /// Only enabled roots participate in path ownership.
    pub enabled: bool,
}

impl Store {
    pub fn list_library_roots(&self) -> Result<Vec<LibraryRoot>, StoreError> {
        self.with_library_read(|db| db.list_library_roots())
    }

    /// Insert a root attached to the kind's default library (extra roots added
    /// through the directory surface land on the default library).
    pub fn insert_library_root(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<LibraryRoot, StoreError> {
        let key = crate::path_key::canonical_root_key(path)?;
        self.with_library_write(|library| library.insert_library_root_prepared(kind, path, &key))
    }

    pub fn get_library_root(&self, id: &str) -> Result<Option<LibraryRoot>, StoreError> {
        self.with_library_read(|db| db.get_library_root(id))
    }

    pub fn delete_extra_library_root(&self, id: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.delete_extra_library_root(id))
    }
}

impl Store {
    /// Record that files under `root_path` must be re-homed to their longest covering root.
    /// Files and their sidecar inputs move in bounded batches. Unidentified facts have
    /// a separate bounded pass before the durable work is acknowledged.
    pub(crate) fn reassign_files_under_root(
        tx: &rusqlite::Connection,
        library_id: &str,
        kind: MediaKind,
        root_key: &str,
    ) -> Result<(), StoreError> {
        let (low, high) = component_range(root_key);
        let mut stmt = tx.prepare("SELECT DISTINCT library_id FROM library_files WHERE kind=?1 AND path_key>=?2 AND path_key<?3 UNION SELECT ?4")?;
        let libraries = stmt
            .query_map(params![kind.as_str(), low, high, library_id], |r| {
                r.get::<_, String>(0)
            })?
            .map(|row| Ok(row?.parse()?))
            .collect::<Result<Vec<_>, StoreError>>()?;
        crate::library_operations::integrity_guard::ensure_integrity_writable(tx, &libraries)?;
        status::register(tx, library_id, kind, root_key)?;
        // NFO inputs move with the file batch that still covers them. Moving every
        // input here lets the first reduce delete sidecars whose files are still queued.
        tx.execute(
            "INSERT INTO pending_root_reassignments (kind, root_key, cursor_key)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(kind, root_key) DO NOTHING",
            params![kind.as_str(), root_key, low],
        )?;
        Ok(())
    }

    /// Drain every pending root reassignment. Each batch is its own transaction;
    /// callers that hold the Store lock across this call keep it for the whole drain.
    pub fn drain_root_reassignments(&self) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.drain_root_reassignments())
    }

    /// One file window. Paused roots rotate behind other work, and only a finite
    /// dispatch window is inspected while the caller holds the Store.
    pub fn drain_one_root_batch(&self) -> Result<Option<bool>, StoreError> {
        self.with_library_write(|db| db.drain_one_root_batch())
    }
}

fn rotate_root(tx: &rusqlite::Connection, kind: &str, root: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE pending_root_reassignments SET dispatch_order=
         (SELECT COALESCE(MAX(dispatch_order),0)+1 FROM pending_root_reassignments)
         WHERE kind=?1 AND root_key=?2",
        params![kind, root],
    )?;
    Ok(())
}

/// Files moved per reassignment transaction.
pub const REASSIGN_BATCH: usize = 100;

thread_local! {
    static BATCH_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Test seam: allow only `budget` more batches on this thread (simulates a crash).
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn set_batch_budget(budget: Option<usize>) {
    BATCH_BUDGET.with(|b| b.set(budget));
}

fn pending_root_work(store: &crate::LibraryConnection<'_>) -> Result<bool, StoreError> {
    Ok(store.library.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_root_reassignments)",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

fn take_batch_budget() -> bool {
    BATCH_BUDGET.with(|b| match b.get() {
        None => true,
        Some(0) => false,
        Some(n) => {
            b.set(Some(n - 1));
            true
        }
    })
}

fn ready_roots(
    tx: &rusqlite::Connection,
    kind: MediaKind,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT r.root_id, r.library_id, r.path_key
         FROM library_roots r JOIN libraries l ON l.library_id = r.library_id
         WHERE l.kind = ?1 AND r.enabled = 1",
    )?;
    Ok(stmt
        .query_map(params![kind.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

type Move = (String, String, String, String, String);

/// One bounded batch after the durable cursor.
///
/// `Some(true)` deletes the cursor because the range is exhausted. `Some(false)` advanced
/// it. `None` means the chosen destination is frozen: the cursor stays and the caller stops.
fn reassign_root_batch(
    tx: &rusqlite::Connection,
    kind: MediaKind,
    root_key: &str,
) -> Result<Option<bool>, StoreError> {
    let cursor: Option<String> = tx
        .query_row(
            "SELECT cursor_key FROM pending_root_reassignments WHERE kind = ?1 AND root_key = ?2",
            params![kind.as_str(), root_key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(cursor) = cursor else {
        return Ok(Some(true));
    };
    let roots = ready_roots(tx, kind)?;
    // Bounded component range on the `files_kind_path` index: `root/` .. `root0`
    // (`'0'` is the byte after `'/'`), so a sibling such as `root-x` is never read.
    let (_, high) = component_range(root_key);
    let candidates: Vec<(String, String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT file_id, library_id, media_id, path_key FROM library_files
             WHERE kind = ?1 AND path_key > ?2 AND path_key < ?3
             ORDER BY path_key LIMIT ?4",
        )?;
        stmt.query_map(
            params![kind.as_str(), cursor, high, REASSIGN_BATCH as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?
    };
    let exhausted = candidates.len() < REASSIGN_BATCH;
    let last = candidates.last().map(|c| c.3.clone());
    let mut moves: Vec<Move> = Vec::new();
    for (file_id, old_library, media_id, path) in &candidates {
        let file_path = Path::new(path);
        let best = roots
            .iter()
            .filter(|(_, _, candidate)| file_path.starts_with(Path::new(candidate)))
            .max_by_key(|(_, _, candidate)| Path::new(candidate).components().count());
        let Some((root_id, new_library, _)) = best else {
            if file_needs_nested_root(tx, kind, path)? {
                return Ok(None);
            }
            continue;
        };
        if old_library != new_library || !file_root_id_is(tx, file_id, root_id)? {
            if !library_is_ready(tx, old_library)? || !library_is_ready(tx, new_library)? {
                // The chosen destination is frozen or otherwise not writable. Leaving the
                // file here and advancing the cursor would permanently skip it (F03).
                return Ok(None);
            }
            moves.push((
                file_id.clone(),
                old_library.clone(),
                media_id.clone(),
                root_id.clone(),
                new_library.clone(),
            ));
        }
    }
    let unidentified_done = if exhausted {
        let Some(done) = unidentified::reassign_batch(tx, kind, root_key)? else {
            return Ok(None);
        };
        done
    } else {
        false
    };
    move_inputs_for_batch(tx, &candidates, &moves, &roots)?;
    apply_moves(tx, moves)?;
    if exhausted && unidentified_done {
        tx.execute(
            "DELETE FROM pending_root_reassignments WHERE kind = ?1 AND root_key = ?2",
            params![kind.as_str(), root_key],
        )?;
    } else if let Some(last) = last {
        tx.execute(
            "UPDATE pending_root_reassignments SET cursor_key = ?3 WHERE kind = ?1 AND root_key = ?2",
            params![kind.as_str(), root_key, last],
        )?;
    }
    Ok(Some(exhausted && unidentified_done))
}

/// Move sidecar inputs that belong to this batch's files, in the same transaction.
///
/// A work/season sidecar that still describes a file staying behind is copied, not moved.
/// Inputs whose files are past the cursor stay where they are so a later reduce cannot
/// treat them as orphans.
fn move_inputs_for_batch(
    tx: &rusqlite::Connection,
    candidates: &[(String, String, String, String)],
    moves: &[Move],
    roots: &[(String, String, String)],
) -> Result<(), StoreError> {
    let destination = |path: &str| -> Option<&(String, String, String)> {
        Path::new(path);
        roots
            .iter()
            .filter(|(_, _, candidate)| Path::new(path).starts_with(Path::new(candidate)))
            .max_by_key(|(_, _, candidate)| Path::new(candidate).components().count())
    };
    let mut moved_files: HashSet<String> = HashSet::new();
    for (file_id, _, _, _, _) in moves {
        moved_files.insert(file_id.clone());
    }
    let mut directories: HashSet<(String, String)> = HashSet::new();
    for (file_id, library_id, _, path) in candidates {
        if !moved_files.contains(file_id) {
            continue;
        }
        if let Some(parent) = Path::new(path).parent() {
            directories.insert((library_id.clone(), parent.to_string_lossy().into_owned()));
        }
    }
    // 同一库的目录合并成一次查询：原来每条目录都扫一遍整个 inputs 集（P7），
    // 而这张表的 PK 以 library_id 打头，一次把本批目录的候选行取回来、
    // 在 Rust 侧分组反而最省。
    let mut by_library: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (library_id, directory) in &directories {
        by_library
            .entry(library_id.as_str())
            .or_default()
            .push(directory.as_str());
    }
    for (library_id, mut dirs) in by_library {
        dirs.sort_unstable();
        for chunk in dirs.chunks(DIRECTORY_CHUNK) {
            // 一行可能同时落在同一批的多条目录下（父目录与子目录都被搬）：
            // 按 input_key 去重，同一个输入只处理一次。
            let mut seen: HashSet<String> = HashSet::new();
            for row in input_rows_under(tx, library_id, chunk)? {
                if !seen.insert(row.input_key.clone()) {
                    continue;
                }
                let Some((new_root, new_library, _)) =
                    destination(&row.path_key).or(destination(&row.input_key))
                else {
                    continue;
                };
                if new_library == library_id && new_root == &row.root_id {
                    continue;
                }
                // 这一行归哪条目录管：取最深的命中目录（都是同一路径的前缀，
                // 更长的更具体）。
                let Some(directory) = deepest_directory(chunk, &row) else {
                    continue;
                };
                let (lower, upper) = directory_range(directory);
                let still_covered: bool = tx.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM library_files
                        WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'
                          AND path_key >= ?3 AND path_key < ?4
                     )",
                    params![library_id, row.media_id, lower, upper],
                    |row| row.get(0),
                )?;
                if still_covered {
                    copy_input_row(tx, library_id, &row.input_key, new_library, new_root)?;
                } else {
                    tx.execute(
                        "UPDATE library_metadata_inputs SET library_id = ?1, root_id = ?2
                         WHERE library_id = ?3 AND input_key = ?4",
                        params![new_library, new_root, library_id, row.input_key],
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// 一条 metadata 输入行的本次搬迁所需的列。
struct InputRow {
    input_key: String,
    path_key: String,
    root_id: String,
    media_id: String,
}

/// 一条语句里最多拼多少条目录（每条目录 3 个绑定参数，远低于 SQLite 的上限）。
const DIRECTORY_CHUNK: usize = 100;

/// 目录下路径的键区间 `[dir/, 上界)`，不含目录本身。
///
/// 上界由 [`crate::path_key::prefix_upper_bound`] 给出，因此这里可以写成能走
/// 索引的范围扫描，而不是 `LIKE dir || '/%'`：后者用不上索引，还会把路径里
/// 真实存在的 `%`/`_` 当成通配符。
fn directory_range(directory: &str) -> (String, String) {
    let lower = if directory.ends_with('/') {
        directory.to_string()
    } else {
        format!("{directory}/")
    };
    let upper = crate::path_key::prefix_upper_bound(&lower);
    (lower, upper)
}

/// 该键是不是「这个目录本身」或「这个目录下的路径」。
fn under_directory(key: &str, directory: &str) -> bool {
    let (lower, upper) = directory_range(directory);
    key == directory || (key >= lower.as_str() && key < upper.as_str())
}

/// 这一行落在哪条目录下：取最深的命中目录（同一个路径的前缀，越长越具体）。
fn deepest_directory<'a>(directories: &[&'a str], row: &InputRow) -> Option<&'a str> {
    directories
        .iter()
        .filter(|directory| {
            under_directory(&row.path_key, directory) || under_directory(&row.input_key, directory)
        })
        .max_by_key(|directory| directory.len())
        .copied()
}

/// 目录下的输入行（`input_key` 或 `path_key` 任一命中）。
///
/// 谓词写成 `= dir` 与 `[dir/, 上界)` 的并集，全部目录并进同一条 SELECT。
fn input_rows_under(
    tx: &rusqlite::Connection,
    library_id: &str,
    directories: &[&str],
) -> Result<Vec<InputRow>, StoreError> {
    if directories.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT input_key, path_key, root_id, media_id FROM library_metadata_inputs
         WHERE library_id = ?1 AND (",
    );
    let mut values: Vec<String> = vec![library_id.to_string()];
    for (index, directory) in directories.iter().enumerate() {
        // ?1 是 library_id；每条目录再占三位：目录本身 / 下界 / 上界。
        let exact = 2 + index * 3;
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!(
            "(input_key = ?{exact} OR (input_key >= ?{lower} AND input_key < ?{upper}))\
             OR (path_key = ?{exact} OR (path_key >= ?{lower} AND path_key < ?{upper}))",
            exact = exact,
            lower = exact + 1,
            upper = exact + 2,
        ));
        let (lower, upper) = directory_range(directory);
        values.push((*directory).to_string());
        values.push(lower);
        values.push(upper);
    }
    sql.push(')');
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            Ok(InputRow {
                input_key: row.get(0)?,
                path_key: row.get(1)?,
                root_id: row.get(2)?,
                media_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn copy_input_row(
    tx: &rusqlite::Connection,
    old_library: &str,
    input_key: &str,
    new_library: &str,
    root_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO library_metadata_inputs
            (library_id, media_id, entity_scope, season, episode, locale, input_key, root_id,
             path_key, parse_state, source_version, fields_json, present_fields_json,
             provenance_json, export_id, export_digest, export_fields_json, revision)
         SELECT ?2, media_id, entity_scope, season, episode, locale, input_key, ?4,
                path_key, parse_state, source_version, fields_json, present_fields_json,
                provenance_json, export_id, export_digest, export_fields_json, revision
         FROM library_metadata_inputs
         WHERE library_id = ?1 AND input_key = ?3",
        params![old_library, new_library, input_key, root_id],
    )?;
    Ok(())
}

fn apply_moves(tx: &rusqlite::Connection, moves: Vec<Move>) -> Result<(), StoreError> {
    let mut affected = HashSet::new();
    let mut shared = HashSet::new();
    for (_, old_library, media_id, root_id, new_library) in &moves {
        if old_library != new_library {
            shared.insert((
                old_library.clone(),
                new_library.clone(),
                media_id.clone(),
                root_id.clone(),
            ));
        }
    }
    for (old_library, new_library, media_id, root_id) in &shared {
        copy_shared_ancestor_inputs(tx, old_library, new_library, media_id, root_id)?;
    }
    for (file_id, old_library, media_id, root_id, new_library) in moves {
        tx.execute(
            "UPDATE library_files SET library_id = ?2, root_id = ?3 WHERE file_id = ?1",
            params![file_id, new_library, root_id],
        )?;
        tx.execute(
            "UPDATE library_file_units SET library_id = ?2 WHERE file_id = ?1",
            params![file_id, new_library],
        )?;
        affected.insert((old_library, media_id.clone()));
        affected.insert((new_library, media_id));
    }
    for (library, media) in affected {
        crate::library_write::reduce::reduce_library_media(tx, library.parse()?, media.parse()?)?;
    }
    Ok(())
}

/// Copy the old Library's work/season inputs whose sidecar sits **above** the new root
/// (a shared `tvshow.nfo`) into the new Library: those sidecars describe the moved files
/// too, so the nested card must not lose them. The parent keeps its row while it still has
/// files under the sidecar's directory (`prune_nfo_inputs` decides on reduce). Provenance,
/// export digest and the imported locale are copied verbatim.
fn copy_shared_ancestor_inputs(
    tx: &rusqlite::Connection,
    old_library: &str,
    new_library: &str,
    media_id: &str,
    root_id: &str,
) -> Result<(), StoreError> {
    const DIR: &str = "rtrim(mi.input_key, replace(mi.input_key, '/', ''))";
    tx.execute(
        &format!(
            "INSERT OR IGNORE INTO library_metadata_inputs
                (library_id, media_id, entity_scope, season, episode, locale, input_key, root_id,
                 path_key, parse_state, source_version, fields_json, present_fields_json,
                 provenance_json, export_id, export_digest, export_fields_json, revision)
             SELECT ?2, mi.media_id, mi.entity_scope, mi.season, mi.episode, mi.locale,
                    mi.input_key, ?4, mi.path_key, mi.parse_state, mi.source_version,
                    mi.fields_json, mi.present_fields_json, mi.provenance_json, mi.export_id,
                    mi.export_digest, mi.export_fields_json, mi.revision
             FROM library_metadata_inputs mi, library_roots r
             WHERE mi.library_id = ?1 AND mi.media_id = ?3
               AND mi.entity_scope IN ('work', 'season')
               AND r.root_id = ?4 AND length({DIR}) > 0
               AND length({DIR}) <= length(r.path_key) + 1
               AND substr(r.path_key || '/', 1, length({DIR})) = {DIR}"
        ),
        params![old_library, new_library, media_id, root_id],
    )?;
    Ok(())
}

fn file_needs_nested_root(
    tx: &rusqlite::Connection,
    kind: MediaKind,
    path: &str,
) -> Result<bool, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT r.path_key FROM library_roots r
         JOIN libraries l ON l.library_id = r.library_id
         WHERE l.kind = ?1 AND r.enabled = 1 AND l.lifecycle_state <> 'ready'",
    )?;
    let roots: Vec<String> = stmt
        .query_map(params![kind.as_str()], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(roots
        .iter()
        .any(|root| Path::new(path).starts_with(Path::new(root))))
}

fn library_is_ready(tx: &rusqlite::Connection, library_id: &str) -> Result<bool, StoreError> {
    let state: Option<String> = tx
        .query_row(
            "SELECT lifecycle_state FROM libraries WHERE library_id = ?1",
            params![library_id],
            |row| row.get(0),
        )
        .optional()?;
    if state.as_deref() != Some("ready") {
        return Ok(false);
    }
    let removing: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_removals
                       WHERE library_id=?1 AND state<>'succeeded')",
        params![library_id],
        |row| row.get(0),
    )?;
    Ok(!removing)
}

fn file_root_id_is(
    tx: &rusqlite::Connection,
    file_id: &str,
    root_id: &str,
) -> Result<bool, StoreError> {
    let current: String = tx.query_row(
        "SELECT root_id FROM library_files WHERE file_id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;
    Ok(current == root_id)
}

/// Half-open key range holding every path strictly below `root_key` on a component
/// boundary.
fn component_range(root_key: &str) -> (String, String) {
    let base = root_key.trim_end_matches('/');
    (format!("{base}/"), format!("{base}0"))
}

#[path = "roots/connection.rs"]
mod connection;
