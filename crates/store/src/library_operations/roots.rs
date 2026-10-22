//! Root-set migration for a reconfiguration operation, one bounded batch at a time.
//!
//! The new root set is materialised as `library_roots` rows under the same Library, then
//! each batch re-parents files, their coverage, scoped metadata inputs and unidentified
//! rows by longest-prefix match. Old roots are only deleted once nothing references them,
//! and no batch ever trusts an in-memory view of the previous pass.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use super::OperationCursor;
use crate::StoreError;

/// Whether the phase has more work after this batch.
pub(crate) enum BatchOutcome {
    More,
    Done,
}

/// Apply one batch of the root change, resuming from `cursor.last_file_id`.
pub(crate) fn apply_root_batch(
    tx: &Connection,
    library: &str,
    requested: &[PathBuf],
    cursor: &mut OperationCursor,
    batch: usize,
) -> Result<BatchOutcome, StoreError> {
    let kind: String = tx.query_row(
        "SELECT kind FROM libraries WHERE library_id = ?1",
        params![library],
        |r| r.get(0),
    )?;
    ensure_root_rows(tx, library, &kind, requested)?;

    if !cursor.files_done {
        let files = next_file_batch(tx, library, cursor.last_file_id.as_deref(), batch)?;
        // Every file of the batch moves; stopping at the page's last id would leave the
        // rest of the page on the previous root generation.
        for file_id in &files {
            reparent_file(tx, library, file_id, requested)?;
        }
        if let Some(last) = files.last() {
            cursor.last_file_id = Some(last.clone());
        }
        let more =
            files.len() >= batch && has_more_files(tx, library, cursor.last_file_id.as_deref())?;
        if more {
            return Ok(BatchOutcome::More);
        }
        cursor.files_done = true;
    }
    // Unidentified rows and metadata inputs are ownership facts of their own: one with no
    // file under it (an unmatched folder, a sidecar whose video is gone) must still move.
    if !cursor.unidentified_done {
        let rows = next_path_batch(
            tx,
            "unidentified",
            "path",
            library,
            cursor.last_unidentified_path.as_deref(),
            batch,
        )?;
        reparent_paths(tx, "unidentified", "path", library, &rows, requested)?;
        if rows.len() >= batch {
            cursor.last_unidentified_path = rows.last().cloned();
            return Ok(BatchOutcome::More);
        }
        cursor.unidentified_done = true;
    }
    let rows = next_path_batch(
        tx,
        "library_metadata_inputs",
        "path_key",
        library,
        cursor.last_input_path.as_deref(),
        batch,
    )?;
    reparent_paths(
        tx,
        "library_metadata_inputs",
        "path_key",
        library,
        &rows,
        requested,
    )?;
    if rows.len() >= batch {
        cursor.last_input_path = rows.last().cloned();
        return Ok(BatchOutcome::More);
    }
    prune_unreferenced_roots(tx, library, requested)?;
    Ok(BatchOutcome::Done)
}

/// One keyset page of distinct path keys of `table` owned by the Library.
fn next_path_batch(
    tx: &Connection,
    table: &str,
    column: &str,
    library: &str,
    after: Option<&str>,
    batch: usize,
) -> Result<Vec<String>, StoreError> {
    let sql = format!(
        "SELECT DISTINCT {column} FROM {table}
         WHERE library_id = ?1 AND (?2 IS NULL OR {column} > ?2)
         ORDER BY {column} ASC LIMIT ?3"
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map(params![library, after, batch as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Move each path row of `table` to its longest matching requested root.
fn reparent_paths(
    tx: &Connection,
    table: &str,
    column: &str,
    library: &str,
    paths: &[String],
    requested: &[PathBuf],
) -> Result<(), StoreError> {
    let sql = format!("UPDATE {table} SET root_id = ?1 WHERE library_id = ?2 AND {column} = ?3");
    for path in paths {
        let Some(root) = longest_root(Path::new(path), requested) else {
            continue;
        };
        let root_id = root_id_for(tx, library, root)?;
        tx.execute(&sql, params![root_id, library, path])?;
    }
    Ok(())
}

pub(crate) fn longest_root<'a>(path: &Path, requested: &'a [PathBuf]) -> Option<&'a PathBuf> {
    requested
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

fn root_id_for(tx: &Connection, library: &str, root: &Path) -> Result<String, StoreError> {
    Ok(tx.query_row(
        "SELECT root_id FROM library_roots WHERE library_id = ?1 AND display_path = ?2",
        params![library, root.to_string_lossy().to_string()],
        |r| r.get(0),
    )?)
}

/// Create the requested roots that do not exist yet, keeping existing ids stable.
fn ensure_root_rows(
    tx: &Connection,
    library: &str,
    kind: &str,
    requested: &[PathBuf],
) -> Result<(), StoreError> {
    let existing: Vec<(String, String)> = {
        let mut stmt =
            tx.prepare("SELECT root_id, display_path FROM library_roots WHERE library_id = ?1")?;
        stmt.query_map(params![library], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (index, path) in requested.iter().enumerate() {
        let display = path.to_string_lossy().to_string();
        let already = existing.iter().any(|(_, candidate)| candidate == &display);
        if already {
            continue;
        }
        let root_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO library_roots
                (root_id, library_id, path_key, display_path, enabled, sort_order)
             VALUES (?1, ?2, ?3, ?3, 1, ?4)",
            params![root_id, library, display, index as i32],
        )?;
        let _ = kind;
    }
    Ok(())
}

/// One keyset page of file ids still owned by the Library.
fn next_file_batch(
    tx: &Connection,
    library: &str,
    after: Option<&str>,
    batch: usize,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT file_id FROM library_files
         WHERE library_id = ?1 AND (?2 IS NULL OR file_id > ?2)
         ORDER BY file_id ASC
         LIMIT ?3",
    )?;
    let ids = stmt
        .query_map(params![library, after, batch as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn has_more_files(tx: &Connection, library: &str, after: Option<&str>) -> Result<bool, StoreError> {
    let exists: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files
                       WHERE library_id = ?1 AND (?2 IS NULL OR file_id > ?2))",
        params![library, after],
        |r| r.get(0),
    )?;
    Ok(exists != 0)
}

/// Re-parent one file (and everything scoped to it) to the longest matching new root.
fn reparent_file(
    tx: &Connection,
    library: &str,
    file_id: &str,
    requested: &[PathBuf],
) -> Result<(), StoreError> {
    let path_key: String = tx.query_row(
        "SELECT path_key FROM library_files WHERE file_id = ?1 AND library_id = ?2",
        params![file_id, library],
        |r| r.get(0),
    )?;
    let file_path = Path::new(&path_key);
    let best = requested
        .iter()
        .filter(|root| file_path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(root) = best else {
        // A file outside every requested root keeps its current owner; the caller
        // decides later whether to remove it, so a reconfiguration never silently
        // deletes library facts.
        return Ok(());
    };
    let display = root.to_string_lossy().to_string();
    let root_id: String = tx.query_row(
        "SELECT root_id FROM library_roots WHERE library_id = ?1 AND display_path = ?2",
        params![library, display],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE library_files SET root_id = ?2 WHERE file_id = ?1",
        params![file_id, root_id],
    )?;
    // Scoped metadata inputs carry the same ownership fact; unidentified rows move in
    // their own pass.
    tx.execute(
        "UPDATE library_metadata_inputs SET root_id = ?2
         WHERE library_id = ?3 AND path_key IN
             (SELECT path_key FROM library_files WHERE file_id = ?1)",
        params![file_id, root_id, library],
    )?;
    Ok(())
}

/// Delete roots of the Library that the requested set no longer contains and that no
/// file, metadata input or unidentified row still references.
fn prune_unreferenced_roots(
    tx: &Connection,
    library: &str,
    requested: &[PathBuf],
) -> Result<(), StoreError> {
    let keep: Vec<String> = requested
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    let existing: Vec<(String, String)> = {
        let mut stmt =
            tx.prepare("SELECT root_id, display_path FROM library_roots WHERE library_id = ?1")?;
        stmt.query_map(params![library], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (root_id, display) in existing {
        if keep.iter().any(|path| path == &display) {
            continue;
        }
        let referenced: i64 = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files WHERE root_id = ?1)
                 OR EXISTS(SELECT 1 FROM library_metadata_inputs WHERE root_id = ?1)
                 OR EXISTS(SELECT 1 FROM unidentified WHERE root_id = ?1)",
            params![root_id],
            |r| r.get(0),
        )?;
        if referenced == 0 {
            tx.execute(
                "DELETE FROM library_roots WHERE root_id = ?1",
                params![root_id],
            )?;
        }
    }
    Ok(())
}

/// Whether some file, unidentified row or metadata input of the Library lies outside
/// every requested root. A root change must not "succeed" while leaving such rows on a
/// root the request dropped, so `begin` refuses it up front.
pub(crate) fn has_uncovered_rows(
    tx: &Connection,
    library: &str,
    requested: &[PathBuf],
) -> Result<bool, StoreError> {
    let roots: Vec<String> = requested
        .iter()
        .map(|root| root.to_string_lossy().trim_end_matches('/').to_string())
        .collect();
    for (table, column) in [
        ("library_files", "path_key"),
        ("unidentified", "path"),
        ("library_metadata_inputs", "path_key"),
    ] {
        let covered: Vec<String> = (0..roots.len())
            .map(|i| {
                let n = i + 2;
                format!("({column} = ?{n} OR substr({column}, 1, length(?{n}) + 1) = ?{n} || '/')")
            })
            .collect();
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {table} WHERE library_id = ?1 AND NOT ({}))",
            covered.join(" OR ")
        );
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&library];
        values.extend(roots.iter().map(|r| r as &dyn rusqlite::ToSql));
        let exists: i64 = tx.query_row(&sql, values.as_slice(), |r| r.get(0))?;
        if exists != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}
