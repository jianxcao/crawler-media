//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn preview_playback_logs(
        &self,
        cutoff: i64,
        batch_size: usize,
    ) -> Result<PlaybackRetentionPreview, StoreError> {
        if batch_size == 0 {
            return Err(StoreError::Invalid(
                "playback retention batch_size must be >= 1".into(),
            ));
        }
        let ids = self.expired_log_ids(cutoff, batch_size)?;
        let truncated = ids.len() > batch_size;
        let scanned = ids.len().min(batch_size);
        Ok(PlaybackRetentionPreview {
            scanned,
            candidates: scanned,
            protected: 0,
            truncated,
        })
    }

    pub fn prune_playback_logs(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<PlaybackRetentionBatch, StoreError> {
        if batch_size == 0 {
            return Err(StoreError::Invalid(
                "playback retention batch_size must be >= 1".into(),
            ));
        }
        self.playback_write(|store| store.prune_playback_logs_batch(cutoff, batch_size, dry_run))
    }

    pub(super) fn prune_playback_logs_batch(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<PlaybackRetentionBatch, StoreError> {
        let ids = self.expired_log_ids(cutoff, batch_size)?;
        let has_more = ids.len() > batch_size;
        let ids = &ids[..ids.len().min(batch_size)];
        let deleted = if dry_run {
            0
        } else {
            self.delete_log_ids(ids)?
        };
        Ok(PlaybackRetentionBatch {
            scanned: ids.len(),
            candidates: ids.len(),
            deleted,
            protected: 0,
            has_more,
        })
    }

    pub(super) fn expired_log_ids(
        &self,
        cutoff: i64,
        batch_size: usize,
    ) -> Result<Vec<String>, StoreError> {
        let limit = i64::try_from(batch_size.saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.library.prepare(
            "SELECT id FROM playback_logs
             WHERE ended_at < ?1
             ORDER BY ended_at ASC, started_at ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cutoff, limit], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>().map_err(Into::into)
    }

    pub(super) fn delete_log_ids(&self, ids: &[String]) -> Result<usize, StoreError> {
        let mut deleted = 0;
        for chunk in ids.chunks(DELETE_CHUNK) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!("DELETE FROM playback_logs WHERE id IN ({marks})");
            deleted += self.library.execute(
                &sql,
                rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
            )?;
        }
        Ok(deleted)
    }
}
