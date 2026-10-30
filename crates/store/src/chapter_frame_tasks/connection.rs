//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn enqueue_chapter_frame_task(&self, task: &ChapterFrameTask) -> Result<bool, StoreError> {
        let targets_json = serde_json::to_string(&task.targets)?;
        let changed = self.library.execute(
            "INSERT INTO chapter_frame_tasks (
                 id, file_id, source_version, targets_json, targets_hash,
                 status, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)
             ON CONFLICT(file_id, source_version, targets_hash) DO NOTHING",
            params![
                task.id,
                compact_file_id(task.file_id),
                task.source_version,
                targets_json,
                task.targets_hash,
                super::super::unix_now() * 1000,
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn pending_chapter_frame_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.library.prepare(
            "SELECT id, file_id, source_version, targets_json, targets_hash
             FROM chapter_frame_tasks
             WHERE status = 'queued'
             ORDER BY created_at_ms, id
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], map_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn claim_chapter_frame_task(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = 'running', started_at_ms = ?2
             WHERE id = ?1 AND status = 'queued'",
            params![id, super::super::unix_now() * 1000],
        )?;
        Ok(changed > 0)
    }

    pub fn finish_chapter_frame_task(
        &self,
        id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let status = if succeeded { "completed" } else { "failed" };
        self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = ?2, error = ?3, finished_at_ms = ?4
             WHERE id = ?1 AND status = 'running'",
            params![id, status, error, super::super::unix_now() * 1000],
        )?;
        Ok(())
    }

    pub fn recover_chapter_frame_tasks(&self) -> Result<usize, StoreError> {
        let changed = self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = 'queued', started_at_ms = NULL
             WHERE status = 'running'",
            [],
        )?;
        Ok(changed)
    }

    pub fn chapter_frame_task_counts(&self) -> Result<ChapterFrameTaskCounts, StoreError> {
        let mut counts = ChapterFrameTaskCounts::default();
        let mut statement = self
            .library
            .prepare("SELECT status, COUNT(*) FROM chapter_frame_tasks GROUP BY status")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            match status.as_str() {
                "queued" => counts.queued = count,
                "running" => counts.running = count,
                "completed" => counts.completed = count,
                "failed" => counts.failed = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    pub fn chapter_frame_task(&self, id: &str) -> Result<Option<ChapterFrameTask>, StoreError> {
        let task = self
            .library
            .query_row(
                "SELECT id, file_id, source_version, targets_json, targets_hash
                 FROM chapter_frame_tasks WHERE id = ?1",
                [id],
                map_task,
            )
            .optional()?;
        Ok(task)
    }
}
