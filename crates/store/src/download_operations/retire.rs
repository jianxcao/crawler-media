use domain::DownloaderId;
use rusqlite::params;

use crate::{Store, StoreError};

impl Store {
    /// Retire unresolved operations, active boost reservations, and non-retained
    /// task/ownership rows belonging to a deleted non-nil downloader.
    ///
    /// Rows with `state IN ('retained_only', 'cleanup_planned')` keep their host
    /// file bytes and ownership intact so host-file cleanup remains possible.
    pub fn retire_deleted_downloader_records(
        &self,
        downloader_id: DownloaderId,
        now_utc: i64,
    ) -> Result<(), StoreError> {
        if downloader_id.as_uuid().is_nil() {
            return Ok(());
        }
        let dl_str = downloader_id.to_string();
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            tx.execute(
                "UPDATE boost_reservations
                 SET stage = 'released',
                     revision = revision + 1,
                     updated_at = ?2
                 WHERE downloader_id = ?1
                   AND stage IN ('provisional', 'final', 'unknown_held')",
                params![dl_str, now_utc],
            )?;
            tx.execute(
                "UPDATE download_operations
                 SET state = 'failed',
                     outcome_code = COALESCE(outcome_code, 'downloader_deleted'),
                     error_message = COALESCE(error_message, 'downloader was deleted'),
                     reconciled_at = CASE
                         WHEN state IN ('submitting', 'unknown') THEN ?2
                         ELSE reconciled_at
                     END,
                     revision = revision + 1,
                     updated_at = ?2
                 WHERE downloader_id = ?1
                   AND state IN ('intent', 'reserved', 'submitting', 'unknown')",
                params![dl_str, now_utc],
            )?;
            tx.execute(
                "UPDATE boost_task_facts
                 SET state = 'cleaned',
                     occupied_bytes = 0,
                     retained_bytes = 0,
                     cleanup_plan_handle = NULL,
                     cleanup_plan_expires_at = NULL,
                     cleanup_plan_json = NULL,
                     revision = revision + 1,
                     updated_at = ?2
                 WHERE downloader_id = ?1
                   AND state IN ('downloading', 'seeding', 'paused')",
                params![dl_str, now_utc],
            )?;
            tx.execute(
                "UPDATE download_ownership
                 SET ownership_state = 'deleted',
                     revision = revision + 1,
                     updated_at = ?2
                 WHERE downloader_id = ?1
                   AND ownership_state IN ('owned', 'unproven')
                   AND NOT EXISTS (
                       SELECT 1 FROM boost_task_facts f
                       WHERE f.downloader_id = download_ownership.downloader_id
                         AND f.ownership_tag = download_ownership.ownership_tag
                         AND f.state IN ('retained_only', 'cleanup_planned')
                   )",
                params![dl_str, now_utc],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
}
