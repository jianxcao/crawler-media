//! Risk 2: a rename / re-parent changes where bytes live, not the bytes. Resume
//! bindings that described the old revision of the same source keep describing it,
//! so they follow the revision bump instead of turning into `SourceChanged`.
//! User summaries are then rebuilt by the per-Library reduction (N04 sparse rows).
use rusqlite::{Connection, params};

use crate::StoreError;

pub(crate) fn carry_resume_bindings(
    tx: &Connection,
    file_id: &str,
    old_revision: i64,
    source_version: &str,
) -> Result<usize, StoreError> {
    let carried = tx.execute(
        "UPDATE playback_units
         SET resume_file_revision = (SELECT file_revision FROM library_files WHERE file_id = ?1)
         WHERE resume_file_id = ?1 AND resume_file_revision = ?2 AND resume_source_version = ?3",
        params![file_id, old_revision, source_version],
    )?;
    if carried > 0 {
        tracing::info!(
            file_id,
            carried,
            "rename carried resume bindings to new file revision"
        );
    }
    Ok(carried)
}
