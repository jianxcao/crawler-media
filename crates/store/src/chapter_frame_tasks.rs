//! Durable chapter scene-frame obligations.
//!
//! One row is one (file, source version, chapter target set) obligation. The
//! request is persisted before any worker starts, so a crash leaves `queued`
//! work recoverable instead of losing it with the in-memory queue.

use domain::LedgerId;
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChapterFrameTask {
    pub id: String,
    pub file_id: LedgerId,
    pub source_version: String,
    pub targets: Vec<(i64, i64)>,
    pub targets_hash: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChapterFrameTaskCounts {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
}

impl ChapterFrameTaskCounts {
    /// Terminal rows (`completed` + `failed`).
    pub fn terminal(&self) -> usize {
        self.completed + self.failed
    }
}

impl Store {
    /// Persist one obligation. `false` means the same
    /// `(file_id, source_version, targets_hash)` already exists, so the
    /// caller must not start the work twice.
    pub fn enqueue_chapter_frame_task(&self, task: &ChapterFrameTask) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.enqueue_chapter_frame_task(task))
    }

    /// Queued obligations in dispatch order. Only `queued` rows are returned;
    /// `running` rows belong to an in-memory worker window.
    pub fn pending_chapter_frame_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        self.with_library_read(|db| db.pending_chapter_frame_tasks(limit))
    }

    /// CAS claim `queued -> running`. `false` means another dispatcher (or a
    /// recovered row) already owns it.
    pub fn claim_chapter_frame_task(&self, id: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.claim_chapter_frame_task(id))
    }

    /// Acknowledge a running obligation. Terminal rows stay in the table so
    /// operators and tests can observe what actually finished.
    pub fn finish_chapter_frame_task(
        &self,
        id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.finish_chapter_frame_task(id, succeeded, error))
    }

    /// Startup recovery: every `running` row belongs to a process that is
    /// gone, so it becomes dispatchable again. Returns the number recovered.
    pub fn recover_chapter_frame_tasks(&self) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.recover_chapter_frame_tasks())
    }

    pub fn chapter_frame_task_counts(&self) -> Result<ChapterFrameTaskCounts, StoreError> {
        self.with_library_read(|db| db.chapter_frame_task_counts())
    }

    pub fn chapter_frame_task(&self, id: &str) -> Result<Option<ChapterFrameTask>, StoreError> {
        self.with_library_read(|db| db.chapter_frame_task(id))
    }
}

fn map_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChapterFrameTask> {
    let file: String = row.get(1)?;
    let targets_json: String = row.get(3)?;
    let targets = serde_json::from_str::<Vec<(i64, i64)>>(&targets_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            targets_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let file_id = LedgerId::from_str(&file).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            file.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(ChapterFrameTask {
        id: row.get(0)?,
        file_id,
        source_version: row.get(2)?,
        targets,
        targets_hash: row.get(4)?,
    })
}

fn compact_file_id(file_id: LedgerId) -> String {
    file_id.to_string().replace('-', "")
}

#[path = "chapter_frame_tasks/connection.rs"]
mod connection;
