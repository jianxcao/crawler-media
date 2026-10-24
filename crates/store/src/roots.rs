use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};
use std::collections::HashSet;

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
        let mut stmt = self.library.prepare(
            "SELECT r.root_id, l.kind, r.display_path,
                    (l.is_default = 1 AND r.sort_order = 0) AS primary_root,
                    r.library_id, r.sort_order, r.enabled
             FROM library_roots r
             JOIN libraries l ON r.library_id = l.library_id
             ORDER BY l.is_default DESC, r.sort_order, r.display_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, String>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, i64>(6)? != 0,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (id, kind, path, is_default, library_id, sort_order, enabled) = row?;
            roots.push(LibraryRoot {
                id,
                kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                path: PathBuf::from(path),
                is_default,
                library_id,
                sort_order,
                enabled,
            });
        }
        Ok(roots)
    }

    /// Insert a root attached to the kind's default library (extra roots added
    /// through the directory surface land on the default library).
    pub fn insert_library_root(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<LibraryRoot, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let library = self
            .default_library(kind)?
            .ok_or_else(|| StoreError::Missing(format!("default {} library", kind.as_str())))?;
        self.ensure_library_mutable(&library.id)?;
        let key = crate::path_key::canonical_root_key(path)?;
        self.ensure_removal_keys_available([key.as_str()])?;
        let sort_order = self.roots_of(&library.id)?.len() as i32;
        let tx = crate::write_tx(&self.library)?;
        tx.execute(
            "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
             VALUES (?1, ?2, ?5, ?3, 1, ?4)",
            params![
                id,
                library.id,
                path,
                sort_order,
                key
            ],
        )?;
        Self::reassign_files_under_root(&tx, &library.id, kind, &key)?;
        tx.commit()?;
        crate::path_key::reload_root_aliases(&self.library)?;
        let _ = self.drain_one_root_batch()?;
        Ok(LibraryRoot {
            id,
            kind,
            path: PathBuf::from(path),
            is_default: false,
            library_id: library.id,
            sort_order,
            enabled: true,
        })
    }

    pub fn get_library_root(&self, id: &str) -> Result<Option<LibraryRoot>, StoreError> {
        Ok(self
            .list_library_roots()?
            .into_iter()
            .find(|root| root.id == id))
    }

    pub fn delete_extra_library_root(&self, id: &str) -> Result<bool, StoreError> {
        let root = match self.get_library_root(id)? {
            Some(r) => r,
            None => return Ok(false),
        };
        if root.is_default {
            return Ok(false);
        }
        self.ensure_library_mutable(&root.library_id)?;
        let n = self
            .library
            .execute("DELETE FROM library_roots WHERE root_id = ?1", params![id])?;
        crate::path_key::reload_root_aliases(&self.library)?;
        Ok(n > 0)
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
        let (low, _high) = component_range(root_key);
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
        let mut batches = 0;
        loop {
            if !pending_root_work(self)? {
                break;
            }
            match self.drain_one_root_batch()? {
                Some(true) => batches += 1,
                _ => break,
            }
        }
        Ok(batches)
    }

    /// One file window. Paused roots rotate behind other work, and only a finite
    /// dispatch window is inspected while the caller holds the Store.
    pub fn drain_one_root_batch(&self) -> Result<Option<bool>, StoreError> {
        let pending: Vec<(String, String)> = {
            let mut statement = self.library.prepare(
                "SELECT kind,root_key FROM pending_root_reassignments
                 ORDER BY dispatch_order,kind,root_key LIMIT 1",
            )?;
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        if pending.is_empty() {
            return Ok(Some(false));
        }
        for (kind, root_key) in pending {
            if !take_batch_budget() {
                return Ok(None);
            }
            let parsed: MediaKind = kind
                .parse()
                .map_err(|_| StoreError::Invalid(format!("reassignment kind {kind}")))?;
            let started = std::time::Instant::now();
            let tx = crate::write_tx(&self.library)?;
            let done = match reassign_root_batch(&tx, parsed, &root_key) {
                Ok(done) => done,
                Err(error) => {
                    tx.rollback()?;
                    let tx = crate::write_tx(&self.library)?;
                    status::update(&tx, parsed, &root_key, "failed", Some(&error.to_string()))?;
                    rotate_root(&tx, &kind, &root_key)?;
                    tx.commit()?;
                    tracing::error!(kind,root_key,%error,"Root reassignment window rejected; retained for retry");
                    return Err(error);
                }
            };
            let state = if done == Some(true) {
                "succeeded"
            } else if done.is_some() {
                "running"
            } else {
                "blocked"
            };
            status::update(&tx, parsed, &root_key, state, None)?;
            rotate_root(&tx, &kind, &root_key)?;
            tx.commit()?;
            if done.is_some() {
                tracing::debug!(
                    kind,
                    root_key,
                    elapsed_ms = started.elapsed().as_millis(),
                    state,
                    "Root reassignment window committed"
                );
                return Ok(Some(true));
            }
            tracing::debug!(
                kind,
                root_key,
                "Root reassignment paused; rotating other roots"
            );
        }
        Ok(None)
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
pub(crate) fn set_batch_budget(budget: Option<usize>) {
    BATCH_BUDGET.with(|b| b.set(budget));
}

fn pending_root_work(store: &Store) -> Result<bool, StoreError> {
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
    for (library_id, directory) in directories {
        let prefix = format!("{directory}/");
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT input_key, path_key, root_id, media_id FROM library_metadata_inputs
                 WHERE library_id = ?1 AND (input_key = ?2 OR input_key LIKE ?3 OR path_key = ?2 OR path_key LIKE ?3)",
            )?;
            stmt.query_map(
                params![library_id, directory, format!("{prefix}%")],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (input_key, path_key, root_id, media_id) in rows {
            let Some((new_root, new_library, _)) =
                destination(&path_key).or(destination(&input_key))
            else {
                continue;
            };
            if new_library == &library_id && new_root == &root_id {
                continue;
            }
            let still_covered: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM library_files
                    WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'
                      AND path_key LIKE ?3
                 )",
                params![library_id, media_id, format!("{prefix}%")],
                |row| row.get(0),
            )?;
            if still_covered {
                copy_input_row(tx, &library_id, &input_key, new_library, new_root)?;
            } else {
                tx.execute(
                    "UPDATE library_metadata_inputs SET library_id = ?1, root_id = ?2
                     WHERE library_id = ?3 AND input_key = ?4",
                    params![new_library, new_root, library_id, input_key],
                )?;
            }
        }
    }
    Ok(())
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
