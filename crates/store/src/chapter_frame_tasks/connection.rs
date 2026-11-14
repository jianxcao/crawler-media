//! Owned snapshots and short SQL persistence; no filesystem observations.
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;

use super::*;

const COLUMNS: &str =
    "id,file_id,source_version,targets_json,targets_hash,generation,attempt,status,results_json";

impl crate::LibraryConnection<'_> {
    pub fn chapter_frame_task_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<ChapterFrameTaskSnapshot>, StoreError> {
        snapshot(self.library, id)
    }

    pub fn current_chapter_frame_task(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<ChapterFrameTaskSnapshot>, StoreError> {
        let id: Option<String> = self.library.query_row(
            "SELECT t.id FROM chapter_frame_tasks t JOIN library_files f ON f.file_id=t.file_id
             WHERE t.file_id=?1 AND t.source_version=f.source_version
               AND t.status NOT IN ('superseded','cancelled') ORDER BY t.created_at_ms DESC,t.id DESC LIMIT 1",
            [file_id.to_string()], |row| row.get(0),
        ).optional()?;
        id.map(|id| snapshot(self.library, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn pending_chapter_frame_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        self.pending_chapter_frame_tasks_after(None, limit)
    }

    pub fn pending_chapter_frame_tasks_after(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        let after_time = match after_id {
            None => i64::MIN,
            Some(id) => match self
                .library
                .query_row(
                    "SELECT created_at_ms FROM chapter_frame_tasks WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?
            {
                Some(time) => time,
                None => return Ok(vec![]),
            },
        };
        let mut statement = self.library.prepare(&format!(
            "SELECT {COLUMNS} FROM chapter_frame_tasks WHERE status='queued'
             AND (created_at_ms,id)>(?1,?2) ORDER BY created_at_ms,id LIMIT ?3"
        ))?;
        let rows = statement.query_map(
            params![
                after_time,
                after_id.unwrap_or(""),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            map_snapshot,
        )?;
        rows.map(|row| Ok(row?.task)).collect()
    }

    pub fn chapter_frame_task_counts(&self) -> Result<ChapterFrameTaskCounts, StoreError> {
        let mut counts = ChapterFrameTaskCounts::default();
        let mut statement = self
            .library
            .prepare("SELECT status,COUNT(*) FROM chapter_frame_tasks GROUP BY status")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })? {
            let (state, count) = row?;
            match state.as_str() {
                "queued" => counts.queued = count,
                "running" => counts.running = count,
                "completed" => counts.completed = count,
                "partial" => counts.partial = count,
                "failed" => counts.failed = count,
                "cancelled" => counts.cancelled = count,
                "superseded" => counts.superseded = count,
                _ => return Err(StoreError::Invalid("unknown chapter frame state".into())),
            }
        }
        Ok(counts)
    }

    pub fn chapter_frame_recovery_tasks(
        &self,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTaskSnapshot>, StoreError> {
        let mut statement = self.library.prepare(&format!(
            "SELECT {COLUMNS} FROM chapter_frame_tasks WHERE id>?1
             AND status NOT IN ('cancelled','superseded') ORDER BY id LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![after_id, i64::try_from(limit).unwrap_or(i64::MAX)],
            map_snapshot,
        )?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}

pub(super) fn snapshot(
    db: &Connection,
    id: &str,
) -> Result<Option<ChapterFrameTaskSnapshot>, StoreError> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM chapter_frame_tasks WHERE id=?1"),
            [id],
            map_snapshot,
        )
        .optional()?)
}

fn decode<T: DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            text.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn map_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChapterFrameTaskSnapshot> {
    let id: String = row.get(0)?;
    let file: String = row.get(1)?;
    let file_id = file.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let state: String = row.get(7)?;
    let mut snapshot = ChapterFrameTaskSnapshot {
        task: ChapterFrameTask {
            id: id.clone(),
            file_id,
            source_version: row.get(2)?,
            targets: decode(&row.get::<_, String>(3)?)?,
            targets_hash: row.get(4)?,
        },
        status: ChapterFrameTaskStatus {
            task_id: id,
            generation: row.get(5)?,
            attempt: row.get(6)?,
            state: decode(&format!("\"{state}\""))?,
            total: 0,
            ready: 0,
            failed: 0,
            targets: vec![],
        },
        targets: decode(&row.get::<_, String>(8)?)?,
    };
    snapshot.refresh_status();
    Ok(snapshot)
}

pub(super) fn save(db: &Connection, snapshot: &ChapterFrameTaskSnapshot) -> Result<(), StoreError> {
    db.execute(
        "UPDATE chapter_frame_tasks SET status=?2,attempt=?3,results_json=?4,
         started_at_ms=CASE WHEN ?2='running' THEN ?5 ELSE NULL END,
         finished_at_ms=CASE WHEN ?2 IN ('queued','running') THEN NULL ELSE ?5 END WHERE id=?1",
        params![
            snapshot.task.id,
            snapshot.status.state.as_str(),
            snapshot.status.attempt,
            serde_json::to_string(&snapshot.targets)?,
            crate::unix_now() * 1000
        ],
    )?;
    Ok(())
}

pub(super) fn next_attempt(attempt: u64) -> Result<u64, StoreError> {
    attempt
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| StoreError::Invalid("chapter frame attempt exhausted".into()))
}
