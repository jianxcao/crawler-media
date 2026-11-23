//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn pending_local_metadata_dispatch_ids(
        &self,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchId>, StoreError> {
        self.pending_local_probe_dispatch_ids(ProbeDispatchStage::Metadata, limit)
    }

    pub fn pending_local_probe_dispatch_ids(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchId>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT u.job_id, u.file_id
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             JOIN library_files f ON f.file_id = u.file_id
             WHERE u.stage = ?1 AND u.status = 'queued'
               AND j.status IN ('queued', 'running')
               AND lower(f.display_path) NOT LIKE '%.strm'
             ORDER BY u.job_id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![stage.as_str(), limit as i64], |row| {
            Ok(ProbeDispatchId {
                job_id: row.get(0)?,
                ledger_id: row.get(1)?,
                stage,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn pending_probe_dispatch_ids_for_jobs(
        &self,
        stage: ProbeDispatchStage,
        job_ids: &[String],
        local_only: bool,
        limit: usize,
    ) -> Result<(Vec<ProbeDispatchId>, Vec<String>), StoreError> {
        let (ids, active_jobs, _) =
            self.pending_probe_dispatch_ids_for_exemptions(stage, job_ids, &[], local_only, limit)?;
        Ok((ids, active_jobs))
    }

    pub fn pending_probe_dispatch_ids_for_exemptions(
        &self,
        stage: ProbeDispatchStage,
        job_ids: &[String],
        unit_pairs: &[(String, String)],
        local_only: bool,
        limit: usize,
    ) -> Result<(Vec<ProbeDispatchId>, Vec<String>, Vec<(String, String)>), StoreError> {
        if job_ids.is_empty() && unit_pairs.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let active_jobs = self.active_exempt_jobs(job_ids)?;
        let active_units = self.active_exempt_units(unit_pairs)?;
        if (active_jobs.is_empty() && active_units.is_empty()) || limit == 0 {
            return Ok((Vec::new(), active_jobs, active_units));
        }
        let active_jobs_json = serde_json::to_string(&active_jobs)?;
        let active_units_json = serde_json::to_string(&active_units)?;
        let sql = if local_only {
            "WITH exempt_units(job_id, file_id) AS (
                 SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]')
                 FROM json_each(?3)
             )
             SELECT job_id, file_id FROM (
                 SELECT u.job_id AS job_id, u.file_id AS file_id
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 JOIN library_files f ON f.file_id = u.file_id
                 WHERE u.job_id IN (SELECT value FROM json_each(?2))
                   AND u.stage = ?1 AND u.status = 'queued'
                   AND j.status IN ('queued', 'running')
                   AND lower(f.display_path) NOT LIKE '%.strm'
                 UNION
                 SELECT u.job_id AS job_id, u.file_id AS file_id
                 FROM exempt_units eu
                 JOIN probe_job_units u ON u.job_id = eu.job_id AND u.file_id = eu.file_id
                 JOIN probe_jobs j ON j.id = u.job_id
                 JOIN library_files f ON f.file_id = u.file_id
                 WHERE u.stage = ?1 AND u.status = 'queued'
                   AND j.status IN ('queued', 'running')
                   AND lower(f.display_path) NOT LIKE '%.strm'
             )
             ORDER BY job_id, file_id
             LIMIT ?4"
        } else {
            "WITH exempt_units(job_id, file_id) AS (
                 SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]')
                 FROM json_each(?3)
             )
             SELECT job_id, file_id FROM (
                 SELECT u.job_id AS job_id, u.file_id AS file_id
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.job_id IN (SELECT value FROM json_each(?2))
                   AND u.stage = ?1 AND u.status = 'queued'
                   AND j.status IN ('queued', 'running')
                 UNION
                 SELECT u.job_id AS job_id, u.file_id AS file_id
                 FROM exempt_units eu
                 JOIN probe_job_units u ON u.job_id = eu.job_id AND u.file_id = eu.file_id
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.stage = ?1 AND u.status = 'queued'
                   AND j.status IN ('queued', 'running')
             )
             ORDER BY job_id, file_id
             LIMIT ?4"
        };
        let mut stmt = self.library.prepare(sql)?;
        let ids = stmt
            .query_map(
                params![
                    stage.as_str(),
                    active_jobs_json,
                    active_units_json,
                    limit as i64
                ],
                |row| {
                    Ok(ProbeDispatchId {
                        job_id: row.get(0)?,
                        ledger_id: row.get(1)?,
                        stage,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((ids, active_jobs, active_units))
    }

    fn active_exempt_jobs(&self, job_ids: &[String]) -> Result<Vec<String>, StoreError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let jobs_json = serde_json::to_string(job_ids)?;
        let mut stmt = self.library.prepare(
            "SELECT id FROM probe_jobs
             WHERE id IN (SELECT value FROM json_each(?1))
               AND status IN ('queued', 'running')",
        )?;
        let active = stmt
            .query_map([&jobs_json], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(active)
    }

    fn active_exempt_units(
        &self,
        unit_pairs: &[(String, String)],
    ) -> Result<Vec<(String, String)>, StoreError> {
        if unit_pairs.is_empty() {
            return Ok(Vec::new());
        }
        let units_json = serde_json::to_string(unit_pairs)?;
        let mut stmt = self.library.prepare(
            "WITH exempt_units(job_id, file_id) AS (
                 SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]')
                 FROM json_each(?1)
             )
             SELECT u.job_id, u.file_id
             FROM exempt_units eu
             JOIN probe_job_units u ON u.job_id = eu.job_id AND u.file_id = eu.file_id
             JOIN probe_jobs j ON j.id = u.job_id
             WHERE u.status IN ('queued', 'running')
               AND j.status IN ('queued', 'running')",
        )?;
        let active = stmt
            .query_map([&units_json], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(active)
    }

    pub fn pending_probe_dispatch_ids(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchId>, StoreError> {
        self.pending_probe_dispatch_ids_with_vm_steps(stage, limit)
            .map(|(ids, _)| ids)
    }

    #[doc(hidden)]
    pub fn pending_probe_dispatch_ids_with_vm_steps(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<(Vec<ProbeDispatchId>, i64), StoreError> {
        if limit == 0 {
            return Ok((Vec::new(), 0));
        }
        let mut statement = self.library.prepare(PENDING_DISPATCH_SQL)?;
        let rows = statement.query_map(params![stage.as_str(), limit as i64], |row| {
            Ok(ProbeDispatchId {
                job_id: row.get(0)?,
                ledger_id: row.get(1)?,
                stage,
            })
        })?;
        let ids = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(statement.get_status(rusqlite::StatementStatus::VmStep));
        Ok((ids, vm_steps))
    }

    pub fn load_probe_dispatch_snapshot(
        &self,
        id: &ProbeDispatchId,
    ) -> Result<Option<ProbeDispatchSnapshot>, StoreError> {
        let unit = self
            .library
            .query_row(
                "SELECT u.job_id, u.file_id, u.kind, u.force_fingerprint,
                        u.reuse_fingerprint_cache, u.overwrite_markers, u.status, u.error,
                        u.sampling_plan_json, u.detection_outcome_json,
                        u.reuse_media_info_cache, u.stage
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.job_id = ?1 AND u.file_id = ?2",
                params![id.job_id, id.ledger_id],
                |row| {
                    Ok(ProbeJobUnit {
                        job_id: row.get(0)?,
                        ledger_id: row.get(1)?,
                        kind: row.get(2)?,
                        force_fingerprint: row.get(3)?,
                        reuse_fingerprint_cache: row.get(4)?,
                        overwrite_markers: row.get(5)?,
                        status: row.get(6)?,
                        error: row.get(7)?,
                        sampling_plan_json: row.get(8)?,
                        detection_outcome_json: row.get(9)?,
                        reuse_media_info_cache: row.get::<_, i64>(10).unwrap_or(0) != 0,
                        stage: row
                            .get::<_, String>(11)
                            .ok()
                            .and_then(|value| ProbeDispatchStage::from_str(&value))
                            .unwrap_or(ProbeDispatchStage::Metadata),
                    })
                },
            )
            .optional()?;
        let Some(unit) = unit else {
            return Ok(None);
        };
        let Some(job) = self.get_probe_job(&id.job_id)? else {
            return Ok(None);
        };
        // `ProbeDispatchId` carries the raw ledger id as text；读路径只接受强类型，
        // 转换失败与「该行不存在」在行为上等价（都是 `None`）。
        let Some(file_id) = id.ledger_id.parse::<domain::LedgerId>().ok() else {
            return Ok(None);
        };
        let Some(row) = self.file_coverage_by_id(file_id)? else {
            return Ok(None);
        };
        let source_version = self
            .library
            .query_row(
                "SELECT source_version FROM file_details WHERE file_id = ?1",
                params![id.ledger_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        Ok(Some(ProbeDispatchSnapshot {
            job,
            unit,
            row,
            source_version,
        }))
    }

    pub fn advance_probe_unit_to_fingerprint(
        &self,
        job_id: &str,
        ledger_id: &str,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_job_units SET status = 'queued', stage = 'fingerprint'
             WHERE job_id = ?1 AND file_id = ?2
               AND status IN ('running', 'queued') AND stage = 'metadata'",
            params![job_id, ledger_id],
        )?;
        Ok(changed > 0)
    }
}
