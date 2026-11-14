//! Claim execution and fenced acknowledgement.
use super::*;

struct RenderSource {
    video: PathBuf,
    duration_ms: Option<i64>,
    lease: Option<Arc<LibraryWorkLease>>,
}

impl ChapterFrameService {
    pub(super) async fn run_task(self: &Arc<Self>, claim: ChapterFrameClaim) {
        let task_started = std::time::Instant::now();
        let task = &claim.task;
        tracing::info!(task=%task.id, file=%task.file_id, generation=%claim.generation,
            attempt=claim.attempt, "章节帧任务开始");
        let resolved = self.resolve_target(task).await;
        let RenderSource {
            video,
            duration_ms,
            lease,
        } = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                let started = std::time::Instant::now();
                let adopted = self
                    .finish_task(&claim, &failed_results(&claim, &error), false)
                    .await;
                self.record_measurement(ChapterFrameMeasurement {
                    file_id: claim.task.file_id,
                    task_id: claim.task.id.clone(),
                    generation: claim.generation.clone(),
                    attempt: claim.attempt,
                    frame_ms: None,
                    publish_ms: Some(started.elapsed().as_millis() as u64),
                    task_ms: task_started.elapsed().as_millis() as u64,
                    adopted,
                });
                self.finish_memory(&claim);
                return;
            }
        };
        let request = render_request(&claim, video, duration_ms);
        let library_control = lease.as_ref().map(|lease| lease.control().clone());
        let control = library_control.clone().unwrap_or_default();
        let runner = self.runner.clone();
        let job_control = control.clone();
        let publish_request = request.clone();
        let publish_control = library_control.clone();
        let execution_lease = lease.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            // Keep lifecycle registration until blocking work actually exits,
            // even if its async dispatcher is aborted. This owns no lock.
            let _lease = execution_lease;
            let started = std::time::Instant::now();
            let staged = runner.render(&request, &job_control);
            let frame_ms = started.elapsed().as_millis() as u64;
            let started = std::time::Instant::now();
            let outcome = staged.and_then(|staged| {
                worker::publish_staged(&staged, &request, publish_control.as_ref(), &job_control)
            });
            (outcome, frame_ms, started.elapsed().as_millis() as u64)
        });

        let outcome = loop {
            tokio::select! {
                joined = &mut worker => break joined,
                _ = tokio::time::sleep(POLL) => {
                    let library_cancelled = library_control
                        .as_ref()
                        .is_some_and(|library| library.is_cancelled());
                    if library_cancelled || self.shutting_down.load(Ordering::SeqCst) {
                        control.cancel();
                    }
                }
            }
        };
        if self.shutting_down.load(Ordering::SeqCst) {
            // Leave the row `running`; shutdown() requeues it after the join.
        } else {
            let (results, frame_ms, file_publish_ms) = match outcome {
                Ok((Ok(results), frame_ms, publish_ms)) => {
                    (results, Some(frame_ms), Some(publish_ms))
                }
                Ok((Err(error), frame_ms, publish_ms)) => (
                    failed_results(&claim, &error),
                    Some(frame_ms),
                    Some(publish_ms),
                ),
                Err(join) => (
                    failed_results(&claim, &format!("chapter frame worker failed: {join}")),
                    None,
                    None,
                ),
            };
            let cancelled = library_control
                .as_ref()
                .is_some_and(|control| control.is_cancelled());
            let publish_started = std::time::Instant::now();
            let adopted = self.finish_task(&claim, &results, cancelled).await;
            self.record_measurement(ChapterFrameMeasurement {
                file_id: claim.task.file_id,
                task_id: claim.task.id.clone(),
                generation: claim.generation.clone(),
                attempt: claim.attempt,
                frame_ms,
                publish_ms: file_publish_ms
                    .map(|elapsed| elapsed + publish_started.elapsed().as_millis() as u64),
                task_ms: task_started.elapsed().as_millis() as u64,
                adopted,
            });
            if adopted == Some(true) && !cancelled {
                self.published_images.fetch_add(
                    results.iter().filter(|result| result.ready).count(),
                    Ordering::SeqCst,
                );
            }
            if adopted.is_some() {
                self.cleanup_retired_candidates(claim.clone(), publish_request)
                    .await;
            }
        }
        self.finish_memory(&claim);
    }

    async fn resolve_target(
        self: &Arc<Self>,
        task: &ChapterFrameTask,
    ) -> Result<RenderSource, String> {
        let service = self.clone();
        let task = task.clone();
        Self::blocking_step(service, move |service| service.resolve_target_sync(&task)).await?
    }

    fn resolve_target_sync(&self, task: &ChapterFrameTask) -> Result<RenderSource, String> {
        let (path, duration_ms, library_id) = {
            let store = self.store.as_ref();
            let Some(row) = store
                .file_coverage_by_id(task.file_id)
                .map_err(|e| e.to_string())?
            else {
                return Err("file_deleted: library file row missing".into());
            };
            (
                row.file.path,
                row.file.duration_ms,
                Some(row.file.library_id.to_string()),
            )
        };
        let lease = match library_id {
            Some(library_id) => Some(Arc::new(
                self.registry
                    .acquire(&self.store, &library_id)
                    .map_err(|error| format!("library frozen: {error}"))?,
            )),
            None => None,
        };
        Ok(RenderSource {
            video: path,
            duration_ms,
            lease,
        })
    }

    /// Write the task's terminal state off the executor. A contended lock keeps the
    /// task un-finished (the durable row is recovered on the next start), so this is
    /// logged, never retried in a loop that would hold the worker.
    async fn finish_task(
        &self,
        claim: &ChapterFrameClaim,
        results: &[ChapterFrameTargetResult],
        cancelled: bool,
    ) -> Option<bool> {
        let id = claim.task.id.clone();
        let attempt = claim.attempt;
        let results = results.to_vec();
        let logged = id.clone();
        let outcome = store_blocking(self.store.clone(), move |store| {
            if cancelled {
                store.cancel_chapter_frame_attempt(&id, attempt)
            } else {
                store.finish_chapter_frame_attempt(&id, attempt, &results)
            }
        })
        .await;
        match outcome {
            Ok(Ok(applied)) => {
                tracing::info!(task=%logged, generation=%claim.generation, attempt, applied, cancelled,
                    "章节帧任务结果已处理");
                Some(applied)
            }
            Ok(Err(error)) => {
                tracing::error!(%error, task = %logged, "章节帧任务终态写入失败");
                None
            }
            Err(error) => {
                tracing::warn!(?error, task = %logged, "章节帧任务终态未写入：worker 失败");
                None
            }
        }
    }
}

fn failed_results(claim: &ChapterFrameClaim, error: &str) -> Vec<ChapterFrameTargetResult> {
    claim
        .targets
        .iter()
        .filter(|target| !target.ready)
        .map(|target| ChapterFrameTargetResult {
            index: target.index,
            ready: false,
            error: Some(error.into()),
        })
        .collect()
}

fn render_request(
    claim: &ChapterFrameClaim,
    video: PathBuf,
    duration_ms: Option<i64>,
) -> ChapterFrameRequest {
    ChapterFrameRequest {
        file_id: claim.task.file_id,
        video,
        duration_ms,
        source_version: claim.task.source_version.clone(),
        targets: claim.task.targets.clone(),
        generation: claim.generation.clone(),
        attempt: claim.attempt,
        pending_indices: claim
            .targets
            .iter()
            .filter(|target| !target.ready)
            .map(|target| target.index)
            .collect(),
        candidates: claim
            .targets
            .iter()
            .filter_map(|target| {
                target
                    .candidate
                    .clone()
                    .map(|artifact| (target.index, artifact))
            })
            .collect(),
    }
}
