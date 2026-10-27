use super::*;

struct DeleteGate {
    path: String,
    store: Arc<Mutex<Store>>,
    remove_ledger: bool,
}
impl api::fingerprint_job::CaptureGate for DeleteGate {
    fn wait_before_capture(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _ = std::fs::remove_file(&self.path);
            if self.remove_ledger {
                self.store.lock().delete_ledger_path(&self.path).unwrap();
            }
        })
    }
}
struct DeleteCapture {
    store: Arc<Mutex<Store>>,
    calls: AtomicUsize,
    delete_during: bool,
    fail: bool,
}
impl FingerprintCaptureEngine for DeleteCapture {
    fn capture_window(&self, r: &CaptureRequest) -> Result<CapturedFingerprint, CaptureFailure> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.delete_during {
            let _ = std::fs::remove_file(&r.path);
            self.store
                .lock()
                .delete_ledger_path(r.path.to_str().unwrap())
                .unwrap();
        }
        if self.fail {
            return Err(CaptureFailure {
                kind: marker::fingerprint::CaptureFailureKind::Io,
                message: "missing source".into(),
                metrics: CaptureMetrics::default(),
            });
        }
        Ok(CapturedFingerprint {
            window: r.window.clone(),
            words: vec![1, 2, 3, 4, 5],
            pcm_duration_ms: Some(r.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}
fn capture_request(row: FileCoverage) -> api::fingerprint_job::EpisodeCaptureRequest {
    api::fingerprint_job::EpisodeCaptureRequest {
        job_id: "delete-job".into(),
        row,
        source_version: "source".into(),
        media_duration_ms: None,
        audio_stream_index: None,
        capture_profile_key: "profile".into(),
        policy: marker::adaptive::SamplingPolicy::default(),
        capture_policy: api::fingerprint_job::CapturePolicy::Recapture,
        templates: Default::default(),
        cost_summary: Default::default(),
    }
}

#[test]
fn cancelled_unit_cannot_reuse_a_cached_fingerprint() {
    let f = setup("uniform", &[1, 2, 3]);
    let row = f.units[0].row.clone();
    let mut request = capture_request(row.clone());
    request.capture_policy = api::fingerprint_job::CapturePolicy::ReuseValid;
    let window = marker::fingerprint::SampleWindow {
        start_ms: 0,
        end_ms: 60_000,
    };
    let ledger_id = row.file.file_id.to_string();
    let other_id = f.units[1].row.file.file_id.to_string();
    let sample = api::store::StoredFingerprintSample {
        sample_id: "cached".into(),
        ledger_id: ledger_id.clone(),
        source_version: "source".into(),
        capture_profile_key: "profile".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 60_000,
        pcm_duration_ms: Some(60_000),
        fingerprint: vec![1, 2, 3],
        captured_job_id: "previous".into(),
        captured_at_ms: 1,
        metrics_json: "{}".into(),
    };
    let attempt = api::store::StoredFingerprintAttempt {
        attempt_id: "cached-attempt".into(),
        job_id: "previous".into(),
        ledger_id: ledger_id.clone(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 60_000,
        phase: "test".into(),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    {
        let store = f.store.lock();
        store
            .complete_fingerprint_attempt(&attempt, Some(&sample))
            .unwrap();
        let specs: Vec<_> = [&ledger_id, &other_id]
            .into_iter()
            .map(|id| store::ProbeJobUnitSpec {
                ledger_id: id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: true,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            })
            .collect();
        assert!(
            store
                .create_probe_job(
                    "delete-job",
                    "media_probe",
                    &f.media_id.to_string(),
                    Some(1),
                    "cache-cancel",
                    &specs
                )
                .unwrap()
        );
        for id in [&ledger_id, &other_id] {
            assert!(store.start_probe_unit("delete-job", id).unwrap());
        }
        store
            .cancel_probe_unit("delete-job", &ledger_id, "source missing")
            .unwrap();
    }
    let ctx = api::fingerprint_job::AdaptiveCaptureContext {
        store: f.store.clone(),
        matcher: f.engine.clone(),
        capture: f.engine.clone(),
        gate: Arc::new(api::fingerprint_job::adaptive::NoopGate),
        timings: None,
    };
    let mut attempts = 0;
    let result = tokio::runtime::Runtime::new().unwrap().block_on(
        api::fingerprint_job::adaptive::capture_or_reuse_segment_sample(
            &ctx,
            &request,
            marker::SegmentKind::Intro,
            &window,
            "test",
            &mut attempts,
            1,
        ),
    );
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|error| error.starts_with("file_deleted")),
        "{result:?}"
    );
    assert_eq!(attempts, 0);
}
#[test]
fn deleting_strm_while_ledger_is_still_present_must_cancel_before_capture() {
    let f = setup("uniform", &[1, 2, 3]);
    let mut row = f.units[0].row.clone();
    let old = row.file.path.clone();
    let renamed = old.to_string_lossy().replace(".mkv", ".strm");
    row.file.path = std::path::PathBuf::from(&renamed);
    std::fs::rename(&old, &row.file.path).unwrap();
    store::test_support::force_file_path(
        &f.store.lock(),
        old.to_str().unwrap(),
        row.file.path.to_str().unwrap(),
    )
    .unwrap();
    let capture = Arc::new(DeleteCapture {
        store: f.store.clone(),
        calls: AtomicUsize::new(0),
        delete_during: false,
        fail: true,
    });
    let ctx = api::fingerprint_job::AdaptiveCaptureContext {
        store: f.store.clone(),
        matcher: f.engine.clone(),
        capture: capture.clone(),
        gate: Arc::new(DeleteGate {
            path: row.file.path.to_string_lossy().into_owned(),
            store: f.store.clone(),
            remove_ledger: false,
        }),
        timings: None,
    };
    let req = capture_request(row);
    let res = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(api::fingerprint_job::capture_episode_adaptive(&ctx, &req));
    println!(
        "STRM deletion result={res:?}; capture reads={}",
        capture.calls.load(Ordering::Relaxed)
    );
    assert!(
        res.as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted")),
        "deleted local strm must be cancelled without waiting for the watcher ledger deletion"
    );
    assert_eq!(capture.calls.load(Ordering::Relaxed), 0);
}
#[test]
fn control_deleting_file_and_ledger_before_capture_cancels() {
    let f = setup("uniform", &[1, 2, 3]);
    let row = f.units[0].row.clone();
    let capture = Arc::new(DeleteCapture {
        store: f.store.clone(),
        calls: AtomicUsize::new(0),
        delete_during: false,
        fail: true,
    });
    let ctx = api::fingerprint_job::AdaptiveCaptureContext {
        store: f.store.clone(),
        matcher: f.engine.clone(),
        capture: capture.clone(),
        gate: Arc::new(DeleteGate {
            path: row.file.path.to_string_lossy().into_owned(),
            store: f.store.clone(),
            remove_ledger: true,
        }),
        timings: None,
    };
    let res = tokio::runtime::Runtime::new().unwrap().block_on(
        api::fingerprint_job::capture_episode_adaptive(&ctx, &capture_request(row)),
    );
    assert!(
        res.as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted"))
    );
    assert_eq!(capture.calls.load(Ordering::Relaxed), 0);
}
#[test]
fn deleting_source_during_capture_must_not_reinsert_sample() {
    let f = setup("uniform", &[1, 2, 3]);
    let row = f.units[0].row.clone();
    let capture = Arc::new(DeleteCapture {
        store: f.store.clone(),
        calls: AtomicUsize::new(0),
        delete_during: true,
        fail: false,
    });
    let ctx = api::fingerprint_job::AdaptiveCaptureContext {
        store: f.store.clone(),
        matcher: f.engine.clone(),
        capture: capture.clone(),
        gate: Arc::new(api::fingerprint_job::adaptive::NoopGate),
        timings: None,
    };
    let res = tokio::runtime::Runtime::new().unwrap().block_on(
        api::fingerprint_job::capture_episode_adaptive(&ctx, &capture_request(row.clone())),
    );
    let st = f.store.lock();
    let samples = st
        .find_covering_fingerprint_samples(&api::store::FingerprintSampleQuery {
            ledger_id: row.file.file_id.to_string(),
            source_version: "source".into(),
            capture_profile_key: "profile".into(),
            kind: "intro".into(),
            window_start_ms: 0,
            window_end_ms: 1,
            captured_job_id: Some("delete-job".into()),
        })
        .unwrap();
    println!(
        "deleted ledger={:?}; result={res:?}; resurrected samples={}",
        st.file_coverage_by_id(row.file.file_id).unwrap(),
        samples.len()
    );
    assert!(
        samples.is_empty(),
        "deleted source must not regain cached fingerprint samples when an already-open read completes"
    );
    assert!(
        res.as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted"))
    );
}

struct EmptyAudioCapture;
impl FingerprintCaptureEngine for EmptyAudioCapture {
    fn capture_window(&self, _: &CaptureRequest) -> Result<CapturedFingerprint, CaptureFailure> {
        Err(CaptureFailure {
            kind: marker::fingerprint::CaptureFailureKind::EmptyAudio,
            message: "no audio frames".into(),
            metrics: CaptureMetrics::default(),
        })
    }
}

#[test]
fn capture_refactor_preserves_persisted_error_kind() {
    let f = setup("uniform", &[1, 2, 3]);
    let ctx = api::fingerprint_job::AdaptiveCaptureContext {
        store: f.store.clone(),
        matcher: f.engine.clone(),
        capture: Arc::new(EmptyAudioCapture),
        gate: Arc::new(api::fingerprint_job::adaptive::NoopGate),
        timings: None,
    };
    let result = tokio::runtime::Runtime::new().unwrap().block_on(
        api::fingerprint_job::capture_episode_adaptive(
            &ctx,
            &capture_request(f.units[0].row.clone()),
        ),
    );
    assert!(result.is_err());
    let attempts = f
        .store
        .lock()
        .list_fingerprint_attempts_for_job("delete-job")
        .unwrap();
    assert!(!attempts.is_empty());
    for attempt in attempts {
        assert_eq!(attempt.status, "failed");
        assert_eq!(attempt.error_kind.as_deref(), Some("empty_audio"));
    }
}
