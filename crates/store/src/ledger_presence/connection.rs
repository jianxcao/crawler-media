use super::*;

impl crate::LibraryConnection<'_> {
    pub fn apply_ledger_presence_with_references(
        &self,
        subscribes: &crate::SubscribeConnection<'_>,
        observations: &[LedgerPresenceObservation],
        mode: LedgerPresenceMode,
        now: i64,
    ) -> Result<LedgerPresenceResult, StoreError> {
        let tx = crate::write_tx(self.library)?;

        let mut result = LedgerPresenceResult::default();
        let mut batch = crate::library_write::presence::PresenceBatch::default();
        let mut seen = HashSet::new();
        for obs in observations {
            if !seen.insert(obs.file.file.file_id) {
                result.skipped += 1;
                continue;
            }
            apply_observation(&tx, obs, mode, now, &mut result, &mut batch)?;
        }
        batch.settle(&tx)?;
        if result.deleted > 0 {
            let candidates: HashSet<domain::MediaId> = observations
                .iter()
                .map(|obs| obs.file.file.media_id)
                .collect();
            let subscribed = subscribes.subscribed_media(&candidates)?;
            crate::library_write::orphan_media::purge_orphan_media_in_tx(
                &tx,
                &candidates,
                &subscribed,
            )?;
        }

        tx.commit()?;
        Ok(result)
    }
}
