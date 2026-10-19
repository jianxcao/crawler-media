//! Transactional cleanup of file-derived rows in library.db for one Library file.
//!
//! Removes probe work items, probe/cache bookkeeping and the `library_files` row
//! itself in a single transaction. Cascades cover coverage (`library_file_units`),
//! details (`file_details`), sidecars (`scrape_sidecars`) and obligations
//! (`file_probe_obligations`); the tables without a file foreign key are handled
//! explicitly below.
//!
//! Does NOT delete Media, Playback, Collections, Markers or disk bytes, and does not
//! reduce summaries: callers use [`crate::library_write::delete_files`] (or
//! [`crate::library_write::delete_files_in_tx`]) when summaries must follow.

use rusqlite::Connection;

use crate::StoreError;

mod probe_unit_retirement;
pub(crate) use probe_unit_retirement::repair_orphan_probe_units;

/// Delete every file-derived row and the file row itself for `file_id` within `conn`.
///
/// Returns `Ok(true)` when a `library_files` row was removed, `Ok(false)` when the
/// file was already gone.
pub(crate) fn delete_file_in_tx(conn: &Connection, file_id: &str) -> Result<bool, StoreError> {
    // Retire queued probe work first: a unit whose file is gone can never run again,
    // and the jobs that derived their counters from it must be resynced.
    probe_unit_retirement::retire_probe_units_for_file(conn, file_id)?;

    conn.execute("DELETE FROM probe_stage_state WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM chapter_frame_tasks WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM fingerprint_cache WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM fingerprint_attempts WHERE file_id = ?1", [file_id])?;
    delete_fingerprint_samples(conn, file_id)?;

    // Cascade-backed rows, deleted explicitly so the cleanup never depends on
    // when the foreign-key action runs.
    conn.execute("DELETE FROM file_details WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM scrape_sidecars WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM file_probe_obligations WHERE file_id = ?1", [file_id])?;

    let changed = conn.execute("DELETE FROM library_files WHERE file_id = ?1", [file_id])?;
    Ok(changed > 0)
}

/// Drop the fingerprint samples of one file and every season model built from them.
fn delete_fingerprint_samples(conn: &Connection, file_id: &str) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM fingerprint_season_models
         WHERE model_id IN (SELECT model_id FROM fingerprint_model_members WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute(
        "DELETE FROM fingerprint_model_members WHERE file_id = ?1",
        [file_id],
    )?;
    conn.execute("DELETE FROM fingerprint_samples WHERE file_id = ?1", [file_id])?;
    Ok(())
}
