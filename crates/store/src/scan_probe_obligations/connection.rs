//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn insert_scanned_files(
        &self,
        rows: &[ScannedLedger],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if rows.len() > MAX_SCAN_BATCH {
            return Err(StoreError::Invalid(format!(
                "exceeded {MAX_SCAN_BATCH} files per scan batch"
            )));
        }
        let contexts = rows
            .iter()
            .map(|row| {
                self.scan_write_context(
                    Path::new(&row.path),
                    media_kind(self.library, row.media_id)?,
                )
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        self.insert_scanned_files_in_contexts(rows, &contexts, now_ms)
    }

    pub fn insert_scanned_files_in_contexts(
        &self,
        rows: &[ScannedLedger],
        contexts: &[crate::ScanWriteContext],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if rows.len() != contexts.len() {
            return Err(StoreError::Invalid(
                "scan context count does not match files".into(),
            ));
        }
        if rows.is_empty() {
            return Ok(());
        }
        if rows.len() > MAX_SCAN_BATCH {
            return Err(StoreError::Invalid(format!(
                "exceeded {MAX_SCAN_BATCH} files per scan batch"
            )));
        }
        let tx = crate::write_tx(self.library)?;
        let mut affected: HashMap<(String, MediaId), BatchChange> = HashMap::new();
        for (scanned, context) in rows.iter().zip(contexts) {
            let kind = media_kind(&tx, scanned.media_id)?;
            context.validate(&tx, Path::new(&scanned.path), kind)?;
            let library_id = context.library_id.to_string();
            ensure_media_writable(&tx, scanned.media_id)?;
            let file = scanned_file(scanned, context);
            insert_file_row(&tx, &file)?;
            let units = if kind == MediaKind::Video {
                vec![UnitKey::WHOLE]
            } else {
                scanned.units.clone()
            };
            sync_coverage(&tx, &file, &units)?;
            let target = crate::metadata::nfo_input::NfoInputTarget::from_file(&file)
                .with_canonical_path(&tx);
            if !crate::metadata::nfo_input::record_nfo_observations(
                &tx,
                &target,
                &scanned.observations,
            )? {
                return Err(StoreError::Protected(format!(
                    "stale NFO observation for {}",
                    scanned.path
                )));
            }
            enqueue_probe(&tx, file.file_id, now_ms)?;
            let change = affected.entry((library_id, scanned.media_id)).or_default();
            change.count += 1;
            if change.count == 1 {
                change.file_id = Some(file.file_id.to_string());
                change.units = units
                    .iter()
                    .map(|unit| (unit.season, unit.episode))
                    .collect();
            } else {
                change.file_id = None;
                change.units.clear();
            }
        }
        for ((library_id, media_id), change) in affected {
            let library_id = library_id.parse::<LibraryId>()?;
            if let Some(file_id) = change.file_id {
                let after = read_file_summary_facts(&tx, &file_id)?
                    .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
                reduce_library_media_incremental(
                    &tx,
                    library_id,
                    media_id,
                    &ChangedFile {
                        before: None,
                        before_units: &[],
                        after: &after,
                        after_units: &change.units,
                    },
                )?;
            } else {
                reduce_library_media(&tx, library_id, media_id)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn scan_probe_obligations(
        &self,
        limit: usize,
    ) -> Result<Vec<ScanProbeObligation>, StoreError> {
        self.scan_probe_obligations_with_vm_steps(limit)
            .map(|(obligations, _)| obligations)
    }

    pub fn scan_probe_obligations_with_vm_steps(
        &self,
        limit: usize,
    ) -> Result<(Vec<ScanProbeObligation>, i64), StoreError> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.library.prepare("SELECT file_id, created_at_ms, reason FROM file_probe_obligations ORDER BY created_at_ms ASC, file_id ASC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit], |row| {
            let id: String = row.get(0)?;
            Ok(ScanProbeObligation {
                ledger_id: LedgerId::from_str(&id)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                created_at_ms: row.get(1)?,
                reason: row.get(2)?,
            })
        })?;
        let obligations = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((obligations, vm_steps))
    }

    pub fn ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM file_probe_obligations WHERE file_id = ?1",
            params![ledger_id.to_string()],
        )?;
        Ok(())
    }

    pub fn ack_scan_probe_obligation_if_current(
        &self,
        obligation: &ScanProbeObligation,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "DELETE FROM file_probe_obligations
             WHERE file_id = ?1 AND created_at_ms = ?2 AND reason = ?3
               AND EXISTS (SELECT 1 FROM library_files f
                           WHERE f.file_id = ?1 AND f.missing_at IS NULL)",
            params![
                obligation.ledger_id.to_string(),
                obligation.created_at_ms,
                obligation.reason
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn ack_scan_probe_obligation_for_work(
        &self,
        obligation: &ScanProbeObligation,
        job_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "DELETE FROM file_probe_obligations
             WHERE file_id = ?1 AND created_at_ms = ?2 AND reason = ?3
               AND EXISTS (SELECT 1 FROM library_files f
                           WHERE f.file_id = ?1 AND f.missing_at IS NULL)
               AND EXISTS (
                   SELECT 1 FROM probe_job_units u JOIN probe_jobs j ON j.id = u.job_id
                   WHERE u.file_id = ?1 AND (
                       (?4 IS NOT NULL AND u.job_id = ?4
                        AND u.status IN ('queued', 'running', 'succeeded', 'failed', 'partial')
                        AND j.status IN ('queued', 'running', 'succeeded', 'failed', 'partial'))
                       OR (?4 IS NULL AND u.status IN ('queued', 'running')
                           AND j.status IN ('queued', 'running'))
                   )
               )",
            params![
                obligation.ledger_id.to_string(),
                obligation.created_at_ms,
                obligation.reason,
                job_id
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn missing_source_probe_unit_refs(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT u.job_id, u.file_id
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             JOIN library_files f ON f.file_id = u.file_id
             WHERE u.status IN ('queued', 'running')
               AND j.status IN ('queued', 'running')
               AND f.missing_at IS NOT NULL
             ORDER BY u.rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
