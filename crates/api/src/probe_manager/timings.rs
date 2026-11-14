use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::store::ProbeJob;

#[derive(Clone, Default)]
pub struct ProbeTimingLedger(Arc<Mutex<TimingState>>);

#[derive(Default)]
struct TimingState {
    active: HashMap<String, ProbeTimingSummary>,
    completed: VecDeque<(String, ProbeTimingSummary)>,
}

#[derive(Default, Clone, Debug, serde::Serialize)]
pub struct ProbeTimingSummary {
    /// False when persisted attempt counters could not be read at retirement.
    pub persisted_attempts_complete: bool,
    pub stage_totals: crate::fingerprint_job::benchmark::BenchmarkStages,
    pub raw_pairs_computed: Option<u64>,
    pub raw_pairs_reused: Option<u64>,
    pub capture_attempts: Vec<crate::fingerprint_job::benchmark::BenchmarkAttempt>,
    #[serde(skip)]
    capture_unknown: bool,
    #[serde(skip)]
    gate_unknown: bool,
    media_info_probes: u64,
    media_info_cache_hits: u64,
    media_info_elapsed_ms: u64,
    intro_reads: u64,
    intro_cache_hits: u64,
    intro_elapsed_ms: u64,
    outro_reads: u64,
    outro_cache_hits: u64,
    outro_elapsed_ms: u64,
    // Whole analysis invocation diagnostics, inclusive of cache lookup/refinement.
    // Actual raw work is recorded separately above and is never added to these spans.
    comparisons: u64,
    comparison_elapsed_ms: u64,
}

impl ProbeTimingLedger {
    pub fn snapshot(&self, job_id: &str) -> Option<ProbeTimingSummary> {
        let state = self.0.lock();
        state.active.get(job_id).cloned().or_else(|| {
            state
                .completed
                .iter()
                .rev()
                .find(|(id, _)| id == job_id)
                .map(|(_, summary)| summary.clone())
        })
    }

    pub(crate) fn record_analysis(
        &self,
        job_id: Option<&str>,
        observation: &crate::fingerprint_job::measurement::ComparisonMeasurement,
    ) {
        self.update(job_id, |summary| {
            *summary.raw_pairs_computed.get_or_insert(0) += observation.computed;
            *summary.raw_pairs_reused.get_or_insert(0) += observation.reused;
            *summary.stage_totals.compare.get_or_insert(0) += observation.compare_ns / 1_000_000;
            *summary.stage_totals.refine.get_or_insert(0) += observation.refine_ns / 1_000_000;
        });
    }

    pub(crate) fn record_capture(
        &self,
        job_id: Option<&str>,
        result: &Result<marker::CapturedFingerprint, marker::CaptureFailure>,
    ) {
        let (metrics, success, error_kind) = match result {
            Ok(capture) => (&capture.metrics, true, None),
            Err(error) => (&error.metrics, false, Some(format!("{:?}", error.kind))),
        };
        self.update(job_id, |summary| {
            summary.capture_unknown |= metrics.capture_process_ms.is_none();
            summary.gate_unknown |= metrics.remote_gate_wait_ms.is_none();
            if !summary.capture_unknown {
                *summary.stage_totals.capture.get_or_insert(0) +=
                    metrics.capture_process_ms.unwrap_or(0);
            } else {
                summary.stage_totals.capture = None;
            }
            if !summary.gate_unknown {
                *summary.stage_totals.remote_gate_wait.get_or_insert(0) +=
                    metrics.remote_gate_wait_ms.unwrap_or(0);
            } else {
                summary.stage_totals.remote_gate_wait = None;
            }
            summary
                .capture_attempts
                .push(crate::fingerprint_job::benchmark::BenchmarkAttempt {
                    operation: "audio_capture".into(),
                    success,
                    duration_ms: metrics.capture_process_ms,
                    error_kind,
                    metrics: serde_json::to_value(metrics).unwrap_or(serde_json::Value::Null),
                });
        });
    }
    pub(super) fn record_media_info(&self, job_id: Option<&str>, elapsed_ms: u64, reused: bool) {
        self.update(job_id, |summary| {
            if reused {
                summary.media_info_cache_hits += 1;
                if !summary.gate_unknown {
                    summary.stage_totals.remote_gate_wait.get_or_insert(0);
                }
            } else {
                summary.media_info_probes += 1;
                summary.media_info_elapsed_ms += elapsed_ms;
                // TracksProbe does not expose its remote gate admission measurement.
                summary.gate_unknown = true;
                summary.stage_totals.remote_gate_wait = None;
            }
        });
    }

    pub(crate) fn record_intro_cache_hit(&self, job_id: Option<&str>) {
        self.update(job_id, |summary| {
            summary.intro_cache_hits += 1;
            summary.record_cached_capture();
        });
    }

    pub(crate) fn record_outro_cache_hit(&self, job_id: Option<&str>) {
        self.update(job_id, |summary| {
            summary.outro_cache_hits += 1;
            summary.record_cached_capture();
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
            if intro_cache_hit || outro_cache_hit {
                summary.record_cached_capture();
            }
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

    pub(crate) fn record_comparison(&self, job_id: Option<&str>, elapsed_ms: u64) {
        self.update(job_id, |summary| {
            summary.comparisons += 1;
            summary.comparison_elapsed_ms += elapsed_ms;
        });
    }

    fn update(&self, job_id: Option<&str>, update: impl FnOnce(&mut ProbeTimingSummary)) {
        let Some(job_id) = job_id else { return };
        let mut state = self.0.lock();
        if !state.completed.iter().any(|(id, _)| id == job_id) {
            update(state.active.entry(job_id.to_string()).or_default());
        }
    }

    fn take(
        &self,
        job_id: &str,
        finalize: impl FnOnce(&mut ProbeTimingSummary),
    ) -> Option<ProbeTimingSummary> {
        let mut state = self.0.lock();
        let mut summary = state.active.remove(job_id)?;
        finalize(&mut summary);
        if state.completed.len() >= 128 {
            state.completed.pop_front();
        }
        state.completed.push_back((job_id.into(), summary.clone()));
        Some(summary)
    }
}

impl ProbeTimingSummary {
    fn record_cached_capture(&mut self) {
        if !self.capture_unknown {
            self.stage_totals.capture.get_or_insert(0);
        }
        if !self.gate_unknown {
            self.stage_totals.remote_gate_wait.get_or_insert(0);
        }
    }
}

pub(super) fn log_terminal_job(
    store: &super::ProbeStoreAccess,
    timings: &ProbeTimingLedger,
    terminal: &ProbeJob,
) {
    finalize_terminal_job(store, timings, terminal);
}

/// Use the owned terminal fact returned by a committed writer transaction.
/// Removing active accounting is the idempotence guard after history eviction.
pub(super) fn finalize_terminal_job(
    store: &super::ProbeStoreAccess,
    timings: &ProbeTimingLedger,
    job: &ProbeJob,
) {
    if job.is_active() {
        return;
    }
    let Some(summary) = terminal_timing_summary(store, timings, &job.id) else {
        return;
    };
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
        intro_reads = ?summary.persisted_attempts_complete.then_some(summary.intro_reads),
        intro_cache_hits = summary.intro_cache_hits,
        intro_elapsed_ms = ?summary.persisted_attempts_complete.then_some(summary.intro_elapsed_ms),
        outro_reads = ?summary.persisted_attempts_complete.then_some(summary.outro_reads),
        outro_cache_hits = summary.outro_cache_hits,
        outro_elapsed_ms = ?summary.persisted_attempts_complete.then_some(summary.outro_elapsed_ms),
        persisted_attempts_complete = summary.persisted_attempts_complete,
        comparisons = summary.comparisons,
        comparison_elapsed_ms = summary.comparison_elapsed_ms,
        raw_pairs_computed = ?summary.raw_pairs_computed,
        raw_pairs_reused = ?summary.raw_pairs_reused,
        "【探测任务】媒体信息、片头片尾采集、跨集比对阶段耗时汇总"
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

/// Merge the in-memory counters with the adaptive attempts persisted for the job.
///
/// Adaptive captures are recorded as fingerprint attempts instead of going through
/// [`ProbeTimingLedger::record_fingerprint`], while the ordinary full-window fallback keeps
/// using the in-memory counters. Both describe real capture work, so they are summed rather
/// than one set replacing the other.
fn terminal_timing_summary(
    store: &super::ProbeStoreAccess,
    timings: &ProbeTimingLedger,
    job_id: &str,
) -> Option<ProbeTimingSummary> {
    let (attempts, complete) = match store.store.list_fingerprint_attempts_for_job(job_id) {
        Ok(attempts) => (attempts, true),
        Err(error) => {
            tracing::warn!(%error, job_id, "探测任务已终结，持久化采集统计不可读；保留已测计时，累计次数未知");
            (vec![], false)
        }
    };
    timings.take(job_id, |summary| {
        summary.persisted_attempts_complete = complete;
        for attempt in &attempts {
            let elapsed_ms = attempt
                .finished_at_ms
                .unwrap_or(attempt.started_at_ms)
                .saturating_sub(attempt.started_at_ms)
                .max(0) as u64;
            match attempt.kind.as_str() {
                "intro" => {
                    summary.intro_reads += 1;
                    summary.intro_elapsed_ms += elapsed_ms;
                }
                "outro" => {
                    summary.outro_reads += 1;
                    summary.outro_elapsed_ms += elapsed_ms;
                }
                _ => {}
            }
        }
    })
}

#[cfg(test)]
mod terminal_timing_tests {
    use super::{ProbeTimingLedger, terminal_timing_summary};
    use crate::Store;
    use crate::store::StoredFingerprintAttempt;
    use std::sync::Arc;

    fn finished_attempt(
        attempt_id: &str,
        job_id: &str,
        kind: &str,
        started_at_ms: i64,
        elapsed_ms: i64,
    ) -> StoredFingerprintAttempt {
        StoredFingerprintAttempt {
            attempt_id: attempt_id.into(),
            job_id: job_id.into(),
            ledger_id: "ledger-1".into(),
            kind: kind.into(),
            window_start_ms: 0,
            window_end_ms: 60_000,
            phase: "fast_verify".into(),
            started_at_ms,
            finished_at_ms: Some(started_at_ms + elapsed_ms),
            status: "failed".into(),
            error_kind: Some("io".into()),
            metrics_json: "{}".into(),
        }
    }

    #[test]
    fn fallback_full_window_reads_are_added_to_adaptive_attempt_counts() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(temp.path()).unwrap());
        let timings = ProbeTimingLedger::default();
        let job_id = "adaptive-fallback-job";

        // Adaptive sampling burned four intro attempts, all failing on IO.
        for index in 0..4 {
            store
                .as_ref()
                .begin_fingerprint_attempt(&finished_attempt(
                    &format!("att-{index}"),
                    job_id,
                    "intro",
                    1_000 + index * 100,
                    10,
                ))
                .unwrap();
        }
        // The ordinary full-window fallback never writes attempts; it reports through the
        // in-memory counters for both the intro and the outro capture.
        timings.record_fingerprint(Some(job_id), false, false, 25, 24);

        let summary = terminal_timing_summary(
            &super::super::ProbeStoreAccess::new(store.clone()),
            &timings,
            job_id,
        )
        .unwrap();

        assert_eq!(summary.intro_reads, 5);
        assert_eq!(summary.intro_elapsed_ms, 4 * 10 + 25);
        assert_eq!(summary.outro_reads, 1);
        assert_eq!(summary.outro_elapsed_ms, 24);
    }
}
