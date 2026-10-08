use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::Store;
use crate::store::ProbeJob;

#[derive(Default)]
pub(super) struct ProbeTimingLedger(Mutex<HashMap<String, ProbeTimingSummary>>);

#[derive(Default)]
struct ProbeTimingSummary {
    media_info_probes: u64,
    media_info_cache_hits: u64,
    media_info_elapsed_ms: u64,
    intro_reads: u64,
    intro_cache_hits: u64,
    intro_elapsed_ms: u64,
    outro_reads: u64,
    outro_cache_hits: u64,
    outro_elapsed_ms: u64,
    comparisons: u64,
    comparison_elapsed_ms: u64,
}

impl ProbeTimingLedger {
    pub(super) fn record_media_info(&self, job_id: Option<&str>, elapsed_ms: u64, reused: bool) {
        self.update(job_id, |summary| {
            if reused {
                summary.media_info_cache_hits += 1;
            } else {
                summary.media_info_probes += 1;
                summary.media_info_elapsed_ms += elapsed_ms;
            }
        });
    }

    pub(super) fn record_fingerprint(
        &self,
        job_id: Option<&str>,
        intro_cache_hit: bool,
        outro_cache_hit: bool,
        intro_elapsed_ms: u64,
        outro_elapsed_ms: u64,
    ) {
        self.update(job_id, |summary| {
            if intro_cache_hit {
                summary.intro_cache_hits += 1;
            } else {
                summary.intro_reads += 1;
                summary.intro_elapsed_ms += intro_elapsed_ms;
            }
            if outro_cache_hit {
                summary.outro_cache_hits += 1;
            } else if outro_elapsed_ms > 0 {
                summary.outro_reads += 1;
                summary.outro_elapsed_ms += outro_elapsed_ms;
            }
        });
    }

    pub(super) fn record_comparison(&self, job_id: Option<&str>, elapsed_ms: u64) {
        self.update(job_id, |summary| {
            summary.comparisons += 1;
            summary.comparison_elapsed_ms += elapsed_ms;
        });
    }

    fn update(&self, job_id: Option<&str>, update: impl FnOnce(&mut ProbeTimingSummary)) {
        let Some(job_id) = job_id else { return };
        update(self.0.lock().entry(job_id.to_string()).or_default());
    }

    fn take(&self, job_id: &str) -> ProbeTimingSummary {
        self.0.lock().remove(job_id).unwrap_or_default()
    }
}

pub(super) fn log_terminal_job(
    store: &Arc<Mutex<Store>>,
    timings: &ProbeTimingLedger,
    fallback: &ProbeJob,
) {
    let job = store
        .lock()
        .get_probe_job(&fallback.id)
        .ok()
        .flatten()
        .unwrap_or_else(|| fallback.clone());
    let summary = timings.take(&job.id);
    let now = now_ms();
    let started = job.started_at_ms.unwrap_or(job.created_at_ms);
    let queue_wait_ms = started.saturating_sub(job.created_at_ms).max(0) as u64;
    let worker_elapsed_ms = job
        .finished_at_ms
        .unwrap_or(now)
        .saturating_sub(started)
        .max(0) as u64;
    tracing::info!(
        job_id = %job.id,
        media_id = %job.media_id,
        season = ?job.season,
        status = %job.status,
        completed = job.completed,
        episodes = job.total,
        failed = job.failed,
        queue_wait_ms,
        worker_elapsed_ms,
        total_elapsed_ms = job.elapsed_ms(now),
        media_info_probes = summary.media_info_probes,
        media_info_cache_hits = summary.media_info_cache_hits,
        media_info_elapsed_ms = summary.media_info_elapsed_ms,
        intro_reads = summary.intro_reads,
        intro_cache_hits = summary.intro_cache_hits,
        intro_elapsed_ms = summary.intro_elapsed_ms,
        outro_reads = summary.outro_reads,
        outro_cache_hits = summary.outro_cache_hits,
        outro_elapsed_ms = summary.outro_elapsed_ms,
        comparisons = summary.comparisons,
        comparison_elapsed_ms = summary.comparison_elapsed_ms,
        "【探测任务】媒体信息、片头片尾采集、跨集比对阶段耗时汇总"
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
