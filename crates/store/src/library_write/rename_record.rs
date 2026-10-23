//! Persisted rename intent rows: decoding and phase/config helpers.
use std::path::PathBuf;

use domain::FileVersion;
use rusqlite::{Connection, params};

use super::rename_durable::{FileRenameRequest, RenameSourceEvidence};
use crate::StoreError;

pub(super) struct RawRename {
    file_id: String,
    from_path: String,
    to_path: String,
    version: FileVersion,
    from_library_id: String,
    from_root_id: String,
    to_library_id: String,
    to_root_id: String,
    sidecars_json: String,
    phase: String,
    evidence: (i64, i64, i64, i64),
    config: (i64, i64),
    media_id: String,
}

impl RawRename {
    pub(super) fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            file_id: r.get(0)?,
            from_path: r.get(1)?,
            to_path: r.get(2)?,
            version: FileVersion {
                revision: r.get(3)?,
                source_version: r.get(4)?,
            },
            from_library_id: r.get(5)?,
            from_root_id: r.get(6)?,
            to_library_id: r.get(7)?,
            to_root_id: r.get(8)?,
            sidecars_json: r.get(9)?,
            phase: r.get(10)?,
            evidence: (r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?),
            config: (r.get(15)?, r.get(16)?),
            media_id: r.get(17)?,
        })
    }

    pub(super) fn into_request(self, rename_id: &str) -> Result<FileRenameRequest, StoreError> {
        let sidecars = serde_json::from_str(&self.sidecars_json)
            .map_err(|e| StoreError::Invalid(format!("sidecars json: {e}")))?;
        let (size, device, inode, mtime_ns) = self.evidence;
        Ok(FileRenameRequest {
            rename_id: rename_id.to_string(),
            file_id: self.file_id.parse()?,
            expected: self.version.clone(),
            from_path: PathBuf::from(self.from_path),
            to_path: PathBuf::from(self.to_path),
            from_library_id: self.from_library_id.parse()?,
            from_root_id: self.from_root_id,
            to_library_id: self.to_library_id.parse()?,
            to_root_id: self.to_root_id,
            sidecars,
            phase: self.phase,
            media_id: self.media_id.parse()?,
            source_evidence: RenameSourceEvidence {
                version: self.version,
                size_bytes: size as u64,
                device: device as u64,
                inode: inode as u64,
                mtime_ns,
            },
            from_config_revision: self.config.0,
            to_config_revision: self.config.1,
        })
    }
}

pub(super) fn config_revision(tx: &Connection, library_id: &str) -> Result<i64, StoreError> {
    Ok(tx.query_row(
        "SELECT config_revision FROM libraries WHERE library_id = ?1",
        params![library_id],
        |r| r.get(0),
    )?)
}

pub(super) fn set_phase(
    conn: &Connection,
    rename_id: &str,
    phase: &str,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE pending_file_renames
         SET phase = ?2, last_completed_phase = ?2, last_error = NULL, updated_at_ms = ?3
         WHERE rename_id = ?1",
        params![rename_id, phase, now],
    )?;
    Ok(())
}
