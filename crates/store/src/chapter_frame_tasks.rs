//! Durable per-target scene-frame obligations and immutable output references.
use domain::LedgerId;

use crate::{Store, StoreError};

mod connection;
mod eligibility;
mod identity;
mod lifecycle;
mod publication;
mod reconcile;
mod status;
pub use identity::chapter_frame_generation;
pub use status::*;

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
    pub partial: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub superseded: usize,
}

impl ChapterFrameTaskCounts {
    pub fn terminal(&self) -> usize {
        self.completed + self.partial + self.failed + self.cancelled + self.superseded
    }
}

impl Store {
    pub fn ensure_chapter_frame_task(
        &self,
        task: &ChapterFrameTask,
        retry_failed: bool,
    ) -> Result<ChapterFrameTaskStatus, StoreError> {
        self.with_library_write(|db| db.ensure_chapter_frame_task(task, retry_failed))
    }

    pub fn pending_chapter_frame_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        self.with_library_read(|db| db.pending_chapter_frame_tasks(limit))
    }

    /// One FIFO keyset page. A deleted cursor yields an empty page so callers
    /// can wrap on the next bounded refill without retaining skipped IDs.
    pub fn pending_chapter_frame_tasks_after(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        self.with_library_read(|db| db.pending_chapter_frame_tasks_after(after_id, limit))
    }

    /// Owned keyset page for lock-free startup validation, including completed
    /// published targets. No directory enumeration belongs in recovery.
    pub fn chapter_frame_recovery_tasks(
        &self,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTaskSnapshot>, StoreError> {
        self.with_library_read(|db| db.chapter_frame_recovery_tasks(after_id, limit))
    }

    pub fn claim_chapter_frame_task(
        &self,
        id: &str,
    ) -> Result<Option<ChapterFrameClaim>, StoreError> {
        self.with_library_write(|db| db.claim_chapter_frame_task(id))
    }

    pub fn finish_chapter_frame_attempt(
        &self,
        id: &str,
        attempt: u64,
        results: &[ChapterFrameTargetResult],
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.finish_chapter_frame_attempt(id, attempt, results))
    }

    pub fn recover_chapter_frame_tasks(&self) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.recover_chapter_frame_tasks())
    }

    pub fn cancel_chapter_frame_attempt(&self, id: &str, attempt: u64) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.cancel_chapter_frame_attempt(id, attempt))
    }

    pub fn chapter_frame_task_counts(&self) -> Result<ChapterFrameTaskCounts, StoreError> {
        self.with_library_read(|db| db.chapter_frame_task_counts())
    }

    pub fn chapter_frame_task_status(
        &self,
        id: &str,
    ) -> Result<Option<ChapterFrameTaskStatus>, StoreError> {
        Ok(self
            .chapter_frame_task_snapshot(id)?
            .map(|snapshot| snapshot.status))
    }

    pub fn chapter_frame_task_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<ChapterFrameTaskSnapshot>, StoreError> {
        self.with_library_read(|db| db.chapter_frame_task_snapshot(id))
    }

    pub fn current_chapter_frame_task(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<ChapterFrameTaskSnapshot>, StoreError> {
        self.with_library_read(|db| db.current_chapter_frame_task(file_id))
    }

    /// File-wide targets composed from current registered physical chapters and
    /// each covered unit's adopted marker. No file or source IO is performed.
    pub fn current_chapter_frame_targets(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<Vec<(i64, i64)>>, StoreError> {
        self.with_library_read(|db| db.current_chapter_frame_targets(file_id))
    }

    /// Caller validates the exact immutable files outside all locks first.
    pub fn repair_missing_chapter_frame_targets(
        &self,
        id: &str,
        generation: &str,
        attempt: u64,
        missing: &[(usize, ChapterFrameArtifact)],
    ) -> Result<ChapterFrameReconcile, StoreError> {
        self.with_library_write(|db| {
            db.repair_missing_chapter_frame_targets(id, generation, attempt, missing)
        })
    }

    /// Startup fences running claims before the caller observes their retained
    /// candidates outside all locks. Adoption compares that recovery fence.
    pub fn adopt_chapter_frame_candidates(
        &self,
        id: &str,
        generation: &str,
        attempt: u64,
        candidates: &[(usize, ChapterFrameArtifact)],
    ) -> Result<ChapterFrameReconcile, StoreError> {
        self.with_library_write(|db| {
            db.adopt_chapter_frame_candidates(id, generation, attempt, candidates)
        })
    }

    /// Retire only outputs owned by this claim which are no longer published
    /// or reserved for recovery. Unlink the returned exact identities afterward.
    pub fn retire_chapter_frame_candidates(
        &self,
        claim: &ChapterFrameClaim,
    ) -> Result<Vec<(usize, ChapterFrameArtifact)>, StoreError> {
        self.with_library_write(|db| db.retire_chapter_frame_candidates(claim))
    }
}
