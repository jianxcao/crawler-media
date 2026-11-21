use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::lifecycle::clock::run_when_all_ready;

struct OneWindow {
    head: bool,
    captures: Arc<Mutex<HashMap<PathBuf, usize>>>,
    /// Optional barrier: hold the 3rd file's tail capture until the first 2
    /// files finish their initial analysis jobs (producing deterministic staggering).
    stagger: Option<(Arc<api::Store>, Vec<String>)>,
    stagger_seen: AtomicUsize,
}

impl OneWindow {
    fn new(head: bool) -> (Self, Arc<Mutex<HashMap<PathBuf, usize>>>) {
        let captures = Arc::new(Mutex::new(HashMap::new()));
        (
            Self {
                head,
                captures: captures.clone(),
                stagger: None,
                stagger_seen: AtomicUsize::new(0),
            },
            captures,
        )
    }

    fn staggered_tails(
        store: Arc<api::Store>,
        file_ids: Vec<String>,
    ) -> (Self, Arc<Mutex<HashMap<PathBuf, usize>>>) {
        let captures = Arc::new(Mutex::new(HashMap::new()));
        (
            Self {
                head: false,
                captures: captures.clone(),
                stagger: Some((store, file_ids)),
                stagger_seen: AtomicUsize::new(0),
            },
            captures,
        )
    }
}

impl marker::FingerprintCaptureEngine for OneWindow {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if (request.window.start_ms == 0) != self.head {
            return Err(marker::CaptureFailure {
                kind: marker::CaptureFailureKind::Io,
                message: "other window capture forbidden".into(),
                metrics: Default::default(),
            });
        }
        if let Some((store, file_ids)) = self.stagger.as_ref() {
            let ordinal = self.stagger_seen.fetch_add(1, Ordering::SeqCst);
            if ordinal == 2 {
                // Wait until the first 2 files' retry jobs finish their initial partial analysis
                // before capturing the 3rd file's tail.
                let deadline = std::time::Instant::now() + Duration::from_secs(15);
                while std::time::Instant::now() < deadline {
                    let succeeded = file_ids
                        .iter()
                        .filter(|id| {
                            store
                                .latest_probe_job_for_ledger(id)
                                .ok()
                                .flatten()
                                .and_then(|job_id| store.get_probe_job(&job_id).ok().flatten())
                                .is_some_and(|job| job.status == "succeeded")
                        })
                        .count();
                    if succeeded >= 2 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        *self
            .captures
            .lock()
            .unwrap()
            .entry(request.path.clone())
            .or_default() += 1;
        marker::FingerprintCaptureEngine::capture_window(&Engine, request)
    }
}

fn with_capture_engine(
    mut evaluation: BenchmarkSession,
    capture: Arc<dyn marker::FingerprintCaptureEngine>,
) -> BenchmarkSession {
    evaluation.state = evaluation
        .state
        .with_probe_engines(Arc::new(Engine), capture, Arc::new(Engine))
        .unwrap();
    evaluation
}

fn prepare_heads_only(root: &std::path::Path) -> (BenchmarkOptions, Vec<(String, Option<String>)>) {
    let mut opts = options(root, &[1, 2, 3]);
    let (head_engine, _) = OneWindow::new(true);
    let failed = run(with_capture_engine(
        session(opts.clone(), false),
        Arc::new(head_engine),
    ));
    assert!(!failed.wallclock_complete);
    opts.run_id = "retry".into();
    opts.scenario = "retry".into();
    let evaluation = session(opts.clone(), false);
    let heads = evaluation
        .registered_files()
        .iter()
        .map(|row| {
            let id = row.file.file_id.to_string();
            let cache = evaluation
                .state
                .store()
                .get_fingerprint_cache(&id)
                .unwrap()
                .unwrap();
            assert!(!cache.intro.words.is_empty());
            assert!(cache.outro.is_none());
            (id, cache.intro.metrics.captured_job_id)
        })
        .collect::<Vec<_>>();
    drop(evaluation);
    (opts, heads)
}

fn assert_common_retry_invariants(
    store: &api::Store,
    run: &api::fingerprint_job::benchmark::BenchmarkRun,
    heads: &[(String, Option<String>)],
    captures: &HashMap<PathBuf, usize>,
) {
    assert!(
        run.wallclock_complete,
        "{:?} {:?}",
        run.media,
        store.recover_probe_jobs().unwrap()
    );
    assert_eq!(run.capture_policy, CapturePolicy::ReuseValid);
    assert!(run.jobs.iter().all(|job| job["status"] == "succeeded"));
    // Exactly one tail capture per file; zero head captures and zero re-reads.
    assert_eq!(captures.len(), 3, "expected 3 files captured: {captures:?}");
    assert!(
        captures.values().all(|&count| count == 1),
        "each file must be captured exactly once (tail only): {captures:?}"
    );
    let mut tail_jobs = Vec::new();
    for (id, previous_job) in heads {
        let cache = store.get_fingerprint_cache(id).unwrap().unwrap();
        assert_eq!(
            cache.intro.metrics.captured_job_id, *previous_job,
            "valid head must be reused for {id}"
        );
        let tail_job = cache.outro.unwrap().metrics.captured_job_id.unwrap();
        assert!(
            run.jobs.iter().any(|job| job["id"] == tail_job),
            "tail capture job {tail_job} missing from associated jobs"
        );
        tail_jobs.push(tail_job);
    }
    assert!(
        run.predictions
            .iter()
            .all(|prediction| prediction.state != "error"),
        "all final predictions must complete cleanly: {:?}",
        run.predictions
    );
    // Verify recorded capture attempts match exactly 3 tail captures (no head or follow-up recapture).
    assert_eq!(run.attempts.len(), 3, "attempts={:?}", run.attempts);
    assert!(
        run.attempts.iter().all(|a| {
            a.success
                && a.operation == "audio_capture"
                && a.metrics["contiguous_pcm_start_ms"]
                    .as_i64()
                    .is_some_and(|start| start > 0)
        }),
        "expected only 3 tail audio_capture attempts: {:?}",
        run.attempts
    );
}

#[test]
fn natural_retry_reuses_valid_heads_and_rejects_claimed_recapture() {
    let root = tempfile::tempdir().unwrap();
    let (opts, heads) = prepare_heads_only(root.path());
    for scenario in ["retry", "incremental"] {
        let mut rejected = opts.clone();
        rejected.scenario = scenario.into();
        rejected.capture_policy = CapturePolicy::Recapture;
        assert!(
            BenchmarkSession::prepare(rejected).is_err(),
            "{scenario} must reject recapture before starting work"
        );
    }
    let (tail_engine, captures) = OneWindow::new(false);
    let evaluation = with_capture_engine(session(opts, false), Arc::new(tail_engine));
    let store = evaluation.state.store().clone();
    // Deterministic simultaneous readiness: hold the first analysis tick until all 3
    // files finish tail capture and enter `stage = 'analysis'`.
    let run = run_when_all_ready(evaluation);
    assert_eq!(
        run.jobs.len(),
        3,
        "simultaneous evidence readiness must batch into 3 jobs: {:?}",
        run.jobs
    );
    assert_common_retry_invariants(&store, &run, &heads, &captures.lock().unwrap());
}

#[test]
fn natural_retry_staggered_evidence_reconciles_analysis_without_recapture() {
    let root = tempfile::tempdir().unwrap();
    let (opts, heads) = prepare_heads_only(root.path());
    let base = session(opts, false);
    let store = base.state.store().clone();
    let file_ids: Vec<String> = heads.iter().map(|(id, _)| id.clone()).collect();
    let (stagger_engine, captures) = OneWindow::staggered_tails(store.clone(), file_ids);
    let evaluation = with_capture_engine(base, Arc::new(stagger_engine));
    let run = run(evaluation);
    // Staggered readiness legitimately spawns analysis-only reconciliation follow-up
    // jobs (preserved in `run.jobs` for auditability) without any audio recapture.
    assert!(
        run.jobs.len() > 3,
        "staggered evidence readiness should record reconciliation follow-up jobs: {:?}",
        run.jobs
    );
    assert_common_retry_invariants(&store, &run, &heads, &captures.lock().unwrap());
}
