use std::str::FromStr;
use rusqlite::{OptionalExtension, params};
use domain::{LedgerId, LedgerRow};
use super::{Store, StoreError};


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProbeObligation {
    pub ledger_id: LedgerId,
    pub created_at_ms: i64,
    pub reason: String,
}

impl Store {
    /// Atomically insert a scanned ledger row and record its probe obligation
    /// in a single Immediate transaction in library.db.
    pub fn insert_scanned_ledger(
        &self,
        row: &LedgerRow,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // Find library and root for row.path
        let lib_info: Option<(String, String, String)> = tx.query_row(
            "SELECT r.library_id, r.root_id, l.kind
             FROM library_roots r
             JOIN libraries l ON r.library_id = l.library_id
             WHERE ?1 LIKE (r.display_path || '%') OR ?1 LIKE (r.path_key || '%')
             ORDER BY LENGTH(r.display_path) DESC LIMIT 1",
            params![row.path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;

        let (lib_id, root_id, kind) = match lib_info {
            Some(info) => info,
            None => {
                // Fallback to first library if path outside roots
                let first: Option<(String, String, String)> = tx.query_row(
                    "SELECT r.library_id, r.root_id, l.kind FROM library_roots r JOIN libraries l ON r.library_id = l.library_id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ).optional()?;
                first.unwrap_or_else(|| ("".into(), "".into(), "movie".into()))
            }
        };

        // Insert into library_files table
        tx.execute(
            "INSERT INTO library_files (
                 file_id, library_id, root_id, media_id, kind, path_key, display_path,
                 presence_state, publication_state, file_revision, source_version,
                 resolution, codec, hdr, quality_source, confidence, filter_score
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 'present', 'published', 1, 'v1', ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(file_id) DO NOTHING",
            params![
                row.id.to_string(),
                lib_id,
                root_id,
                row.media_id.to_string(),
                kind,
                row.path,
                row.resolution,
                row.codec,
                row.hdr,
                row.quality_source.as_str(),
                row.confidence.as_str(),
                row.filter_score,
            ],
        )?;

        // Insert unit mapping
        let season_num = row.season.map(|s| s as i32).unwrap_or(-1);
        let episode_num = row.episode.map(|e| e as i32).unwrap_or(-1);
        tx.execute(
            "INSERT INTO library_file_units (file_id, library_id, media_id, season, episode)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(file_id, season, episode) DO NOTHING",
            params![row.id.to_string(), lib_id, row.media_id.to_string(), season_num, episode_num],
        )?;

        // Insert probe obligation into file_probe_obligations
        tx.execute(
            "INSERT INTO file_probe_obligations (file_id, created_at_ms, reason)
             VALUES (?1, ?2, 'scan')
             ON CONFLICT(file_id) DO NOTHING",
            params![row.id.to_string(), now_ms],
        )?;

        // Maintain library_items via reduce_library_media
        if let Ok(lid) = lib_id.parse::<domain::LibraryId>() {
            crate::library_write::reduce_library_media(&tx, lid, row.media_id)?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Retrieve up to `limit` (clamped 1..=100) pending probe obligations in deterministic order.
    pub fn scan_probe_obligations(&self, limit: usize) -> Result<Vec<ScanProbeObligation>, StoreError> {
        self.scan_probe_obligations_with_vm_steps(limit)
            .map(|(obligations, _)| obligations)
    }

    /// Same read as [`Self::scan_probe_obligations`], additionally reporting the
    /// SQLite VM steps the query consumed so tests can assert DB work.
    #[doc(hidden)]
    pub fn scan_probe_obligations_with_vm_steps(
        &self,
        limit: usize,
    ) -> Result<(Vec<ScanProbeObligation>, i64), StoreError> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.library.prepare(
            "SELECT file_id, created_at_ms, reason
             FROM file_probe_obligations
             ORDER BY created_at_ms ASC, file_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            let id_str: String = row.get(0)?;
            let created_at_ms: i64 = row.get(1)?;
            let reason: String = row.get(2)?;
            let ledger_id = LedgerId::from_str(&id_str)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
            Ok(ScanProbeObligation {
                ledger_id,
                created_at_ms,
                reason,
            })
        })?;
        let obligations = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((obligations, vm_steps))
    }

    /// Acknowledge / remove a fulfilled probe obligation.
    pub fn ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM file_probe_obligations WHERE file_id = ?1",
            params![ledger_id.to_string()],
        )?;
        Ok(())
    }
}
