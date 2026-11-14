use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct MeasuredCapture(AtomicBool);
impl marker::FingerprintCaptureEngine for MeasuredCapture {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if self.0.load(Ordering::SeqCst) && request.path.to_string_lossy().contains("S01E01") {
            return Err(marker::CaptureFailure {
                kind: marker::CaptureFailureKind::Decode,
                message: "controlled capture failure".into(),
                metrics: marker::CaptureMetrics {
                    capture_process_ms: Some(7),
                    ..Default::default()
                },
            });
        }
        FixedCandidate.capture_window(request)
    }
}

async fn terminal_refresh(s: &ProbeScenario) -> store::ProbeJob {
    refresh_with_capture_policy(s, false).await
}
async fn refresh_with_capture_policy(s: &ProbeScenario, recapture: bool) -> store::ProbeJob {
    assert_eq!(
        s.manager.enqueue_marker_refresh(
            s.ledgers
                .iter()
                .map(|row| {
                    let mut unit = unit(row, true);
                    unit.reuse_fingerprint_cache = !recapture;
                    unit
                })
                .collect()
        ),
        s.ledgers.len()
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
            {
                // The terminal DB commit precedes off-writer timing/log finalization.
                tokio::task::yield_now().await;
                return job;
            }
            advance_analysis(s);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analysis_terminal_jobs_retire_into_bounded_recent_timing_history() {
    let mut s = scenario().await;
    s.manager.shutdown_workers().await.unwrap();
    let capture = Arc::new(MeasuredCapture(AtomicBool::new(false)));
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), Arc::new(FixedCandidate), capture.clone())
            .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    // An ordinary uncertain result must retire just like a verified refresh.
    for row in &s.ledgers {
        s.store
            .put_cached_chapters(
                &row.file.file_id.to_string(),
                &[marker::ChapterMarker {
                    start_ms: 20_000,
                    end_ms: 95_000,
                    title: None,
                    marker_type: Some(marker::MarkerType::IntroStart),
                    synthetic: false,
                }],
            )
            .unwrap();
    }
    assert!(s.manager.enqueue(unit(&s.ledgers[0], false)));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if s.store
                .latest_probe_job_for_scope(&format!("ledger:{}", s.ledgers[0].file.file_id))
                .unwrap()
                .is_some_and(|j| !j.is_active())
            {
                break;
            }
            advance_analysis(&s);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let ordinary = s
        .store
        .latest_probe_job_for_scope(&format!("ledger:{}", s.ledgers[0].file.file_id))
        .unwrap()
        .unwrap();
    assert_eq!(latest_outcome(&s)["status"], "uncertain");
    let unresolved = terminal_refresh(&s).await;
    assert_eq!(unresolved.status, "succeeded");
    assert!(
        s.manager
            .timing_snapshot(&ordinary.id)
            .unwrap()
            .raw_pairs_computed
            .is_some()
    );
    assert!(
        s.manager
            .timing_snapshot(&unresolved.id)
            .unwrap()
            .raw_pairs_computed
            .is_some()
    );
    capture.0.store(true, Ordering::SeqCst);
    let capture_failed = refresh_with_capture_policy(&s, true).await;
    assert_eq!(capture_failed.status, "failed");
    assert!(
        s.manager
            .timing_snapshot(&capture_failed.id)
            .unwrap()
            .capture_attempts
            .iter()
            .any(|a| !a.success)
    );
    capture.0.store(false, Ordering::SeqCst);
    for row in &s.ledgers {
        s.store
            .put_cached_chapters(&row.file.file_id.to_string(), &[])
            .unwrap();
    }
    let mut recent = String::new();
    for _ in 0..129 {
        let job = terminal_refresh(&s).await;
        assert_eq!(job.status, "succeeded", "{job:?}");
        recent = job.id;
    }
    s.manager.shutdown_workers().await.unwrap();
    assert!(
        s.manager
            .timing_snapshot(&recent)
            .unwrap()
            .raw_pairs_reused
            .is_some()
    );
    assert!(
        s.manager.timing_snapshot(&ordinary.id).is_none(),
        "old ordinary Analysis timing must leave active accounting and bounded recent history"
    );
    assert!(
        s.manager.timing_snapshot(&unresolved.id).is_none(),
        "uncertain Analysis refresh must also retire"
    );
    assert!(
        s.manager.timing_snapshot(&capture_failed.id).is_none(),
        "capture-failed refresh must also retire"
    );
}
