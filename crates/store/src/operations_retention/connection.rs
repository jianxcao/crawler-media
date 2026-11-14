//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn prune_terminal_operations(
        &self,
        cutoff_ms: i64,
        batch: usize,
        dry_run: bool,
    ) -> Result<PruneCounts, StoreError> {
        if batch == 0 {
            return Err(StoreError::Invalid(
                "operation retention batch must be >= 1".into(),
            ));
        }
        let batch = batch.min(MAX_BATCH);
        if dry_run {
            // Read connection: a preview must not even take the write lock.
            let candidates = select_candidates(self.library, cutoff_ms, batch)?;
            return Ok(PruneCounts {
                examined: candidates.len(),
                deleted: 0,
                tables: Vec::new(),
            });
        }
        let tx = crate::write_tx(self.library)?;
        // Select inside the same IMMEDIATE transaction that deletes: another connection
        // cannot slip a row into the window between selection and deletion.
        let candidates = select_candidates(&tx, cutoff_ms, batch)?;
        let mut counts = PruneCounts {
            examined: candidates.len(),
            deleted: 0,
            tables: Vec::new(),
        };
        for candidate in &candidates {
            let journal = Journal::owning_terminal(JOURNALS[candidate.journal]);
            if delete_candidate(&tx, &journal, &candidate.id)? {
                counts.deleted += 1;
                match counts
                    .tables
                    .iter_mut()
                    .find(|(table, _)| *table == journal.table)
                {
                    Some((_, deleted)) => *deleted += 1,
                    None => counts.tables.push((journal.table, 1)),
                }
            }
        }
        tx.commit()?;
        if counts.deleted > 0 {
            tracing::debug!(
                cutoff_ms,
                examined = counts.examined,
                deleted = counts.deleted,
                tables = ?counts.tables,
                "已清理超龄的终态 operation 行"
            );
        }
        Ok(counts)
    }
}
