use super::*;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

struct PressureMatcher(Arc<AtomicBool>);
impl FingerprintEngine for PressureMatcher {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<CommonSegment> {
        self.0.store(true, Ordering::SeqCst);
        FixedCandidate.find_common_segment(a, b, min, max)
    }
}
struct MeasuredRms {
    armed: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
}
impl marker::FingerprintCaptureEngine for MeasuredRms {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let mut capture = FixedCandidate.capture_window(request)?;
        capture.metrics.pcm_frame_ms = 20;
        capture.metrics.pcm_rms_dbfs_milli = vec![-20_000; 9000];
        if self.fail.load(Ordering::SeqCst) {
            self.armed.store(true, Ordering::SeqCst);
            return Err(marker::CaptureFailure {
                kind: marker::CaptureFailureKind::Decode,
                message: "controlled capture failure".into(),
                metrics: capture.metrics,
            });
        }
        Ok(capture)
    }
}

fn exhaust_at_terminal_commit(
    store: &store::Store,
    armed: Arc<AtomicBool>,
    held: Arc<Mutex<Vec<store::ReaderLease>>>,
) {
    let pool = store.reader_pool(store::Db::Library);
    // Prewarm outside the writer; hooks only withdraw an admitted idle lease.
    drop(pool.checkout(std::time::Duration::from_secs(1)).unwrap());
    let changed = Arc::new(AtomicBool::new(false));
    store
        .with_writer(store::Db::Library, |db| {
            let observed = changed.clone();
            let ready = armed.clone();
            db.update_hook(Some(move |_, _: &str, table: &str, _| {
                if table == "probe_jobs" && ready.load(Ordering::SeqCst) {
                    observed.store(true, Ordering::SeqCst);
                }
            }));
            db.commit_hook(Some(move || {
                if changed.swap(false, Ordering::SeqCst)
                    && armed.swap(false, Ordering::SeqCst)
                    && let Ok(lease) = pool.checkout(std::time::Duration::from_secs(1))
                {
                    held.lock().push(lease);
                }
                false
            }));
            Ok(())
        })
        .unwrap();
}
fn remove_pressure(store: &store::Store, held: &Mutex<Vec<store::ReaderLease>>) {
    held.lock().clear();
    store
        .with_writer(store::Db::Library, |db| {
            db.commit_hook(None::<fn() -> bool>);
            db.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>);
            Ok(())
        })
        .unwrap();
}
async fn terminal(s: &ProbeScenario, observer: &store::Store) -> store::ProbeJob {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if let Some(job) = observer
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
            {
                return job;
            }
            s.clock.set(s.clock.now_ms() + 2_000);
            s.manager.wake_dispatch();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}
async fn age_history(s: &ProbeScenario, observer: &store::Store) {
    for _ in 0..129 {
        assert_eq!(
            s.manager.enqueue_marker_refresh_with_policy(
                vec![unit(&s.ledgers[0], true)],
                api::fingerprint_job::CapturePolicy::ReuseValid
            ),
            1
        );
        assert_eq!(terminal(s, observer).await.status, "succeeded");
    }
}
async fn pressure_case(failure: bool) {
    let mut s = scenario().await;
    s.manager.shutdown_workers().await.unwrap();
    let observer = store::Store::open(s.tmp.path()).unwrap();
    s.store
        .configure_reader_pool_size(store::Db::Library, std::num::NonZeroUsize::new(1).unwrap())
        .unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let held = Arc::new(Mutex::new(Vec::new()));
    let fail = Arc::new(AtomicBool::new(failure));
    exhaust_at_terminal_commit(&s.store, armed.clone(), held.clone());
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(PressureMatcher(armed.clone())),
            Arc::new(MeasuredRms {
                armed,
                fail: fail.clone(),
            }),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    assert_eq!(
        s.manager
            .enqueue_marker_refresh(vec![unit(&s.ledgers[0], true)]),
        1
    );
    let job = terminal(&s, &observer).await;
    assert_eq!(job.status, if failure { "failed" } else { "succeeded" });
    assert_eq!(
        held.lock().len(),
        1,
        "pressure must cover the terminal commit"
    );
    // Independent readers observe completion while the manager's read admission
    // expires at its existing five-second bound. No durable work is rerun.
    tokio::time::sleep(std::time::Duration::from_millis(5_300)).await;
    remove_pressure(&s.store, &held);
    let recent = s.manager.timing_snapshot(&job.id).unwrap();
    assert!(
        !recent.persisted_attempts_complete,
        "unread persisted counters remain explicitly unknown"
    );
    assert_eq!(recent.capture_attempts.len(), if failure { 1 } else { 2 });
    assert!(recent.capture_attempts.iter().all(|attempt| {
        attempt.metrics["pcm_rms_dbfs_milli"]
            .as_array()
            .is_some_and(|rms| !rms.is_empty())
    }));
    let id = s.ledgers[0].file.file_id.to_string();
    let captured = s.store.get_fingerprint_cache(&id).unwrap();
    fail.store(false, Ordering::SeqCst);
    age_history(&s, &observer).await;
    s.manager.shutdown_workers().await.unwrap();
    assert_eq!(
        s.store.get_fingerprint_cache(&id).unwrap(),
        captured,
        "postcommit pressure must not rerun captured work or restamp its owner"
    );
    assert!(
        s.manager.timing_snapshot(&job.id).is_none(),
        "postcommit read failure must not strand RMS in active accounting beyond bounded history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_analysis_retires_measured_payload_even_when_postcommit_readers_fail() {
    pressure_case(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_terminal_completion_keeps_owned_fact_when_postcommit_readers_fail() {
    pressure_case(true).await;
}
