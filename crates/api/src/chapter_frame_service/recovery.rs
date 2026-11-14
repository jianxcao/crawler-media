//! Recovery and explicit missing-target repair observe immutable files only
//! after the Store has returned an owned snapshot and released its connections.
use super::*;

impl ChapterFrameService {
    pub(super) fn recover_sync(&self) -> Result<(), String> {
        self.store
            .recover_chapter_frame_tasks()
            .map_err(|error| error.to_string())?;
        let mut after = String::new();
        loop {
            let page = self
                .store
                .chapter_frame_recovery_tasks(&after, DISPATCH_WINDOW)
                .map_err(|error| error.to_string())?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.task.id.clone();
            for snapshot in page {
                self.recover_snapshot_sync(snapshot)?;
            }
        }
        Ok(())
    }

    fn recover_snapshot_sync(
        &self,
        mut snapshot: store::ChapterFrameTaskSnapshot,
    ) -> Result<(), String> {
        let Some(row) = self
            .store
            .file_coverage_by_id(snapshot.task.file_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let video = row.file.path;
        if crate::fingerprint_job::current_source_version(&video) != snapshot.task.source_version {
            return Ok(());
        }
        let candidates = snapshot
            .targets
            .iter()
            .filter_map(|target| {
                let artifact = target.candidate.as_ref()?;
                worker::valid_frame(&chapter_frame_artifact_path(
                    &video,
                    &snapshot.status.generation,
                    target.index,
                    artifact,
                ))
                .then(|| (target.index, artifact.clone()))
            })
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            match self.store.adopt_chapter_frame_candidates(
                &snapshot.task.id,
                &snapshot.status.generation,
                snapshot.status.attempt,
                &candidates,
            ) {
                Ok(outcome) if outcome.applied => {}
                // Deferred adoption must not suppress independent missing-output
                // repair. Its CAS still checks this owned snapshot's exact fence.
                Ok(_) => return self.repair_missing_sync(&snapshot, &video),
                Err(store::StoreError::Missing(_)) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
            snapshot = match self
                .store
                .chapter_frame_task_snapshot(&snapshot.task.id)
                .map_err(|error| error.to_string())?
            {
                Some(current) => current,
                None => return Ok(()),
            };
        }
        self.repair_missing_sync(&snapshot, &video)
    }

    pub(super) fn repair_missing_sync(
        &self,
        snapshot: &store::ChapterFrameTaskSnapshot,
        video: &Path,
    ) -> Result<(), String> {
        let missing = snapshot
            .targets
            .iter()
            .filter_map(|target| {
                let artifact = target.published.as_ref()?;
                (!worker::valid_frame(&chapter_frame_artifact_path(
                    video,
                    &snapshot.status.generation,
                    target.index,
                    artifact,
                )))
                .then(|| (target.index, artifact.clone()))
            })
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            match self.store.repair_missing_chapter_frame_targets(
                &snapshot.task.id,
                &snapshot.status.generation,
                snapshot.status.attempt,
                &missing,
            ) {
                Ok(_) | Err(store::StoreError::Missing(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    pub(super) async fn cleanup_retired_candidates(
        &self,
        claim: ChapterFrameClaim,
        request: ChapterFrameRequest,
    ) {
        let result = store_blocking(self.store.clone(), move |store| {
            store.retire_chapter_frame_candidates(&claim)
        })
        .await;
        match result {
            Ok(Ok(retired)) => {
                let _ = tokio::task::spawn_blocking(move || {
                    worker::cleanup_candidates(&request, &retired)
                })
                .await;
            }
            error => {
                tracing::warn!(?error, file=%request.file_id, "章节帧候选退休失败，保留文件供恢复")
            }
        }
    }
}
