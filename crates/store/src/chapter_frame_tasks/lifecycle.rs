use rusqlite::{OptionalExtension, params};

use super::connection::{next_attempt, save, snapshot};
use super::eligibility::{Eligibility, assess, inspect};
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn ensure_chapter_frame_task(
        &self,
        task: &ChapterFrameTask,
        retry_failed: bool,
    ) -> Result<ChapterFrameTaskStatus, StoreError> {
        self.ensure_chapter_frame_task_inner(task, retry_failed, false)
    }

    pub(super) fn ensure_chapter_frame_task_inner(
        &self,
        task: &ChapterFrameTask,
        retry_failed: bool,
        fresh_publication: bool,
    ) -> Result<ChapterFrameTaskStatus, StoreError> {
        let targets = identity::normalize_targets(&task.targets)?;
        let generation = chapter_frame_generation(task.file_id, &task.source_version, &targets, 1);
        let tx = crate::write_tx(self.library)?;
        if matches!(inspect(&tx, task)?, Eligibility::Fenced(_)) {
            return Err(StoreError::Protected(
                "chapter frame source/Library is not current and writable".into(),
            ));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM chapter_frame_tasks WHERE file_id=?1 AND generation=?2
                 AND (?3=0 OR status NOT IN ('cancelled','superseded'))
                 ORDER BY (status IN ('cancelled','superseded')),created_at_ms DESC,id DESC LIMIT 1",
                params![task.file_id.to_string(), generation, fresh_publication],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let mut current = snapshot(&tx, &id)?.ok_or_else(|| StoreError::Missing(id))?;
            if retry_failed
                && matches!(
                    current.status.state,
                    ChapterFrameState::Partial | ChapterFrameState::Failed
                )
            {
                for target in current.targets.iter_mut().filter(|target| !target.ready) {
                    target.error = None;
                    target.candidate = None;
                }
                current.status.state = ChapterFrameState::Queued;
                current.refresh_status();
                save(&tx, &current)?;
            }
            tx.commit()?;
            return Ok(current.status);
        }
        tx.execute(
            "UPDATE chapter_frame_tasks SET status='superseded',attempt=attempt+1,finished_at_ms=?2
             WHERE file_id=?1 AND status NOT IN ('cancelled','superseded')",
            params![task.file_id.to_string(), crate::unix_now() * 1000],
        )?;
        let target_states = (0..targets.len())
            .map(|index| ChapterFrameTargetSnapshot {
                index,
                ready: false,
                error: None,
                published: None,
                candidate: None,
            })
            .collect::<Vec<_>>();
        let state = if targets.is_empty() {
            ChapterFrameState::Completed
        } else {
            ChapterFrameState::Queued
        };
        let status = ChapterFrameTaskStatus {
            task_id: task.id.clone(),
            generation: generation.clone(),
            attempt: 0,
            state,
            total: targets.len(),
            ready: 0,
            failed: 0,
            targets: target_states
                .iter()
                .map(|target| ChapterFrameTargetResult {
                    index: target.index,
                    ready: false,
                    error: None,
                })
                .collect(),
        };
        if !targets.is_empty() {
            tx.execute(
                "INSERT INTO chapter_frame_tasks (id,file_id,source_version,targets_json,targets_hash,
                 generation,attempt,results_json,status,created_at_ms) VALUES (?1,?2,?3,?4,?5,?5,0,?6,'queued',?7)",
                params![task.id, task.file_id.to_string(), task.source_version, serde_json::to_string(&targets)?,
                    generation, serde_json::to_string(&target_states)?, crate::unix_now() * 1000],
            )?;
        }
        tx.commit()?;
        Ok(status)
    }

    pub fn claim_chapter_frame_task(
        &self,
        id: &str,
    ) -> Result<Option<ChapterFrameClaim>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let Some(mut current) = snapshot(&tx, id)? else {
            return Ok(None);
        };
        if current.status.state != ChapterFrameState::Queued {
            return Ok(None);
        }
        if assess(&tx, &mut current)? != Eligibility::Ready {
            tx.commit()?;
            return Ok(None);
        }
        current.status.attempt = next_attempt(current.status.attempt)?;
        current.status.state = ChapterFrameState::Running;
        for target in current.targets.iter_mut().filter(|target| !target.ready) {
            target.candidate = Some(ChapterFrameArtifact {
                attempt: current.status.attempt,
                id: uuid::Uuid::new_v4().simple().to_string(),
            });
            target.error = None;
        }
        save(&tx, &current)?;
        tx.commit()?;
        Ok(Some(ChapterFrameClaim {
            task: current.task,
            generation: current.status.generation,
            attempt: current.status.attempt,
            targets: current.targets,
        }))
    }

    pub fn finish_chapter_frame_attempt(
        &self,
        id: &str,
        attempt: u64,
        results: &[ChapterFrameTargetResult],
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let Some(mut current) = snapshot(&tx, id)? else {
            return Ok(false);
        };
        if current.status.state != ChapterFrameState::Running || current.status.attempt != attempt {
            return Ok(false);
        }
        match assess(&tx, &mut current)? {
            Eligibility::Ready => {}
            Eligibility::Deferred => {
                current.status.state = ChapterFrameState::Queued;
                current.status.attempt = next_attempt(current.status.attempt)?;
                for target in current.targets.iter_mut().filter(|target| !target.ready) {
                    target.candidate = None;
                }
                save(&tx, &current)?;
                tx.commit()?;
                return Ok(false);
            }
            Eligibility::Fenced(_) => {
                tx.commit()?;
                return Ok(false);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for result in results {
            if result.index >= current.targets.len()
                || !seen.insert(result.index)
                || (result.ready && result.error.is_some())
            {
                return Err(StoreError::Invalid("invalid_chapter_frame_result".into()));
            }
            let target = &mut current.targets[result.index];
            if target.ready {
                continue;
            }
            if result.ready {
                let artifact = target
                    .candidate
                    .take()
                    .filter(|artifact| artifact.attempt == attempt)
                    .ok_or_else(|| {
                        StoreError::Invalid("unregistered_chapter_frame_artifact".into())
                    })?;
                target.published = Some(artifact);
            } else {
                target.candidate = None;
            }
            target.ready = result.ready;
            target.error = if result.ready {
                None
            } else {
                Some(
                    result
                        .error
                        .clone()
                        .unwrap_or_else(|| "render_failed".into()),
                )
            };
        }
        for target in current
            .targets
            .iter_mut()
            .filter(|target| !target.ready && !seen.contains(&target.index))
        {
            target.error = Some("missing_render_result".into());
            target.candidate = None;
        }
        current.status.state = current.terminal_state();
        save(&tx, &current)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn recover_chapter_frame_tasks(&self) -> Result<usize, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let ids = tx
            .prepare("SELECT id FROM chapter_frame_tasks WHERE status='running'")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in &ids {
            if let Some(mut current) = snapshot(&tx, id)?
                && !matches!(assess(&tx, &mut current)?, Eligibility::Fenced(_))
            {
                current.status.state = ChapterFrameState::Queued;
                current.status.attempt = next_attempt(current.status.attempt)?;
                save(&tx, &current)?;
            }
        }
        tx.commit()?;
        Ok(ids.len())
    }

    pub fn cancel_chapter_frame_attempt(&self, id: &str, attempt: u64) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let Some(mut current) = snapshot(&tx, id)? else {
            return Ok(false);
        };
        if current.status.attempt != attempt || current.status.state.fenced() {
            return Ok(false);
        }
        current.status.state = ChapterFrameState::Cancelled;
        current.status.attempt = next_attempt(attempt)?;
        save(&tx, &current)?;
        tx.commit()?;
        Ok(true)
    }
}
