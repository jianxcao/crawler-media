//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn enqueue_scan(
        &self,
        library_id: &str,
        signature: &str,
        targets: &[PathBuf],
        now_ms: i64,
    ) -> Result<ScanOperation, StoreError> {
        let tx = crate::write_tx(self.library)?;
        // 三个职责共用同一个 IMMEDIATE 事务边界：读活动 operation → 合并意图或新建。
        let op = match active_scan_operation(&tx, library_id)? {
            Some(existing) => merge_scan_intent(&tx, existing, targets)?,
            None => create_scan_operation(&tx, library_id, signature, targets, now_ms)?,
        };
        tx.commit()?;
        Ok(op)
    }

    pub fn scan_operation(&self, id: &str) -> Result<Option<ScanOperation>, StoreError> {
        self.library
            .query_row(
                &format!("SELECT {SCAN_OPERATION_COLUMNS} FROM scan_operations WHERE id = ?1"),
                [id],
                map_scan_operation,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn scan_operations_for_library(
        &self,
        library_id: &str,
    ) -> Result<Vec<ScanOperation>, StoreError> {
        let mut stmt = self.library.prepare(&format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations WHERE library_id = ?1
             ORDER BY created_at_ms DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([library_id], map_scan_operation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn advance_scan_consumed_rev(&self, id: &str, rev: i64) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET consumed_rev = ?1 WHERE id = ?2 AND consumed_rev < ?1",
            params![rev, id],
        )?;
        Ok(changed > 0)
    }

    pub fn cancel_scan(&self, id: &str, _now_ms: i64) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = 'cancelled'
             WHERE id = ?1 AND status IN ('queued', 'running', 'yielded')",
            [id],
        )?;
        Ok(changed > 0)
    }

    pub fn update_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = ?1, report_json = ?2
             WHERE id = ?3 AND status NOT IN ('cancelled', 'completed', 'failed')",
            params![status.as_str(), report.to_string(), id],
        )?;
        Ok(changed > 0)
    }

    pub fn set_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = ?1
             WHERE id = ?2 AND status NOT IN ('cancelled', 'completed', 'failed')",
            params![status.as_str(), id],
        )?;
        Ok(changed > 0)
    }

    pub fn finalize_or_requeue_scan(
        &self,
        id: &str,
        expected_dirty_rev: i64,
        terminal_status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<Option<i64>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let current: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT status, dirty_rev, consumed_rev FROM scan_operations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((status_raw, dirty_rev, consumed_rev)) = current else {
            return Ok(None);
        };
        let status =
            ScanOperationStatus::from_str(&status_raw).unwrap_or(ScanOperationStatus::Queued);
        if !status.is_active() {
            return Ok(None);
        }
        if dirty_rev != expected_dirty_rev {
            let consumed = consumed_rev.max(dirty_rev);
            tx.execute(
                "UPDATE scan_operations
                 SET status = 'queued', consumed_rev = ?1, report_json = ?2
                 WHERE id = ?3 AND status IN ('queued', 'running', 'yielded')",
                params![consumed, report.to_string(), id],
            )?;
            tx.commit()?;
            return Ok(Some(consumed));
        }
        let changed = tx.execute(
            "UPDATE scan_operations SET status = ?1, report_json = ?2
             WHERE id = ?3 AND status IN ('queued', 'running', 'yielded') AND dirty_rev = ?4",
            params![
                terminal_status.as_str(),
                report.to_string(),
                id,
                expected_dirty_rev
            ],
        )?;
        if changed > 0 {
            tx.execute("DELETE FROM scan_seen WHERE operation_id = ?1", [id])?;
            tx.commit()?;
            return Ok(None);
        }
        tx.commit()?;
        Ok(None)
    }

    pub fn arm_scan_probe_obligation(
        &self,
        ledger_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO file_probe_obligations (file_id, created_at_ms, reason)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(file_id) DO UPDATE SET
                 created_at_ms = excluded.created_at_ms, reason = excluded.reason",
            params![ledger_id, now_ms, reason],
        )?;
        Ok(())
    }

    pub fn claim_next_queued_scan(
        &self,
        excluding: &[String],
    ) -> Result<Option<ScanOperation>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let mut query = format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations
             WHERE status = 'queued'"
        );
        if !excluding.is_empty() {
            let placeholders: Vec<String> =
                (1..=excluding.len()).map(|i| format!("?{}", i)).collect();
            query.push_str(&format!(" AND id NOT IN ({})", placeholders.join(",")));
        }
        query.push_str(" ORDER BY created_at_ms ASC, rowid ASC LIMIT 1");

        let mut stmt = tx.prepare(&query)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        for id in excluding {
            params.push(id);
        }
        let op_opt = stmt
            .query_row(rusqlite::params_from_iter(params), map_scan_operation)
            .optional()?;
        drop(stmt);

        if let Some(mut op) = op_opt {
            let updated = tx.execute(
                "UPDATE scan_operations SET status = 'running' WHERE id = ?1 AND status = 'queued'",
                params![op.id],
            )?;
            if updated > 0 {
                tx.commit()?;
                op.status = ScanOperationStatus::Running;
                return Ok(Some(op));
            }
        }
        Ok(None)
    }

    pub fn recover_orphaned_scans(&self) -> Result<usize, StoreError> {
        let running: Vec<(String, String)> = {
            let mut stmt = self
                .library
                .prepare("SELECT id, library_id FROM scan_operations WHERE status = 'running'")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut list = Vec::new();
            for r in rows {
                list.push(r?);
            }
            list
        };

        let mut recovered = 0;
        for (op_id, lib_id) in running {
            let accepts = match self.library_accepts_work(&lib_id) {
                Ok(accepts) => accepts,
                Err(error) => {
                    // 读不到冻结状态时保守地当作不可工作（cancel），但必须留痕。
                    tracing::error!(%error, library_id = %lib_id, %op_id, "恢复扫描时读取 Library 工作状态失败，按不可工作处理");
                    false
                }
            };
            if accepts {
                let updated = self.library.execute(
                    "UPDATE scan_operations SET status = 'queued' WHERE id = ?1 AND status = 'running'",
                    params![op_id],
                )?;
                recovered += updated;
            } else if let Err(error) = self.library.execute(
                "UPDATE scan_operations SET status = 'cancelled' WHERE id = ?1 AND status = 'running'",
                params![op_id],
            ) {
                tracing::error!(%error, library_id = %lib_id, %op_id, "恢复扫描时取消失效 operation 失败");
            }
        }
        Ok(recovered)
    }
}
