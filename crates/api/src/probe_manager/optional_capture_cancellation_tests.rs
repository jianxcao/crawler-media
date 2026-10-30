use crate::scrape_store::ScrapeStoreExt;
use marker::fingerprint::capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
    FingerprintCaptureEngine,
};

struct OptionalCapture {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<marker::WorkControl>>>,
    released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    stopped: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl FingerprintCaptureEngine for OptionalCapture {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.capture_window_controlled(request, &marker::WorkControl::default())
    }

    fn capture_window_controlled(
        &self,
        request: &CaptureRequest,
        control: &marker::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.send(control.clone());
            let (released, changed) = self.released.as_ref();
            let mut guard = released.lock().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !*guard && !control.is_cancelled() && std::time::Instant::now() < deadline {
                let result = changed
                    .wait_timeout(guard, std::time::Duration::from_millis(10))
                    .unwrap();
                guard = result.0;
            }
        }
        if control.is_cancelled() {
            if let Some(stopped) = self.stopped.lock().take() {
                let _ = stopped.send(());
            }
            return Err(CaptureFailure {
                kind: CaptureFailureKind::Cancelled,
                message: "optional detection cancelled".into(),
                metrics: CaptureMetrics::default(),
            });
        }
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}

impl marker::FingerprintEngine for OptionalCapture {
    fn extract_at(
        &self,
        path: &std::path::Path,
        start_secs: u32,
        duration_secs: u32,
        control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        let request = CaptureRequest {
            path: path.to_path_buf(),
            window: marker::SampleWindow::new(
                i64::from(start_secs) * 1_000,
                (i64::from(start_secs) + i64::from(duration_secs)) * 1_000,
            ),
            audio_stream_index: None,
            process_deadline_ms: 0,
        };
        self.capture_window_controlled(&request, control)
            .map(|capture| capture.words)
            .map_err(|error| error.to_string())
    }

    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

struct ReleaseOptionalCapture(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReleaseOptionalCapture {
    fn drop(&mut self) {
        let (released, changed) = self.0.as_ref();
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

struct OfflineOptionalFetcher;
impl indexer::Fetcher for OfflineOptionalFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch(
            "offline capture fixture".into(),
        ))
    }
}

fn optional_capture_state(
    harness: &RefreshHarness,
    capture: Arc<OptionalCapture>,
    sampling_mode: &str,
) -> crate::ApiState {
    let mut state = crate::ApiState::new_arc(
        harness.store.clone(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(OfflineOptionalFetcher),
        Arc::new(downloader::MemoryDownloader::new(
            harness._tmp.path().join("stage"),
        )),
        harness._tmp.path().join("library"),
    )
    .unwrap();
    let manager = Arc::new(ProbeManager::with_engines(
        harness.store.clone(),
        capture.clone(),
        capture,
    ).unwrap());
    manager.set_library_registry(state.library_work.clone());
    state.probe = manager;
    {
        let store = harness.store.lock();
        store
            .set_library_intro_settings(&harness.library_id, true, true)
            .unwrap();
        store
            .save_scrape_config(&crate::scrape_config::ScrapeConfigSetting {
                fingerprint_sampling_mode: Some(sampling_mode.into()),
                ..Default::default()
            })
            .unwrap();
    }
    state
}

async fn patch_optional_capture(state: &crate::ApiState, library: &str, field: &str) {
    use tower::ServiceExt;
    let response = crate::router(state.clone())
        .oneshot(
            axum::http::Request::patch(format!("/api/v1/libraries/{library}"))
                .header("Authorization", "Bearer management-secret")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({field: false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

fn optional_switch_stops_running_capture(field: &str, sampling_mode: &str) {
    // ApiState is built before the runtime so fixture construction does not start workers.
    let harness = RefreshHarness::new();
    let (entered, captured) = tokio::sync::oneshot::channel();
    let (stopped, stopped_capture) = tokio::sync::oneshot::channel();
    let released = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let capture = Arc::new(OptionalCapture {
        entered: Mutex::new(Some(entered)),
        released: released.clone(),
        stopped: Mutex::new(Some(stopped)),
    });
    let state = optional_capture_state(&harness, capture, sampling_mode);
    let ordinary = state.acquire_library_work(&harness.library_id).unwrap();
    let release = ReleaseOptionalCapture(released);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut unit = harness.make_units().remove(0);
        unit.marker_refresh_id = None;
        let job = state
            .probe
            .enqueue_direct_fingerprint(
                unit.clone(),
                Some(1_200_000),
                unit.row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        assert!(state.probe.try_start_workers());
        let control = tokio::time::timeout(std::time::Duration::from_secs(2), captured)
            .await
            .unwrap()
            .unwrap();
        patch_optional_capture(&state, &harness.library_id, field).await;
        let was_cancelled = control.is_cancelled();
        let ordinary_survived = !ordinary.control().is_cancelled();
        let stopped_without_release =
            tokio::time::timeout(std::time::Duration::from_millis(200), stopped_capture)
                .await
                .is_ok();
        drop(release); // Always release the finite fake before assertions/runtime teardown.
        let terminal = wait_for_terminal(&harness.store, &job).await;
        state.probe.shutdown_workers().await.unwrap();
        assert!(
            was_cancelled,
            "PATCH must reach the actual running capture control"
        );
        assert!(
            stopped_without_release,
            "capture must stop without a source release"
        );
        assert!(
            ordinary_survived,
            "mandatory metadata/scan lease must survive"
        );
        assert_eq!(terminal.status, "cancelled");
        harness.assert_old_markers_intact();
        assert!(
            harness
                .store
                .lock()
                .get_fingerprint_cache(&unit.row.file.file_id.to_string())
                .unwrap()
                .is_none()
        );
        assert!(state.acquire_library_work(&harness.library_id).is_ok());
    });
}

#[test]
fn disabling_intro_detection_stops_running_capture_before_publication() {
    optional_switch_stops_running_capture("detect_intros", "adaptive");
}

#[test]
fn disabling_fingerprint_stops_running_capture_before_publication() {
    optional_switch_stops_running_capture("enable_fingerprint", "adaptive");
}

#[test]
fn disabling_intro_detection_stops_running_full_window_capture_before_publication() {
    optional_switch_stops_running_capture("detect_intros", "full_window");
}

#[test]
fn disabling_fingerprint_stops_running_full_window_capture_before_publication() {
    optional_switch_stops_running_capture("enable_fingerprint", "full_window");
}

fn mixed_owner_optional_cancellation(target_completed: bool) {
    let harness = RefreshHarness::new();
    let sibling_root = harness._tmp.path().join("mixed-sibling");
    std::fs::create_dir_all(&sibling_root).unwrap();
    let mut sibling_row = super::super::test_support::coverage(
        harness.media_id,
        &sibling_root.join("Pantheon.S01E03.mkv").to_string_lossy(),
        Some(1),
        Some(3),
    );
    std::fs::write(&sibling_row.file.path, b"finite sibling source").unwrap();
    let sibling_id = {
        let store = harness.store.lock();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Sibling",
                &[sibling_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .set_library_intro_settings(&library.id, true, true)
            .unwrap();
        super::super::test_support::publish_into(&store, &mut sibling_row);
        library.id
    };
    let capture = Arc::new(OptionalCapture {
        entered: Mutex::new(None),
        released: Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new())),
        stopped: Mutex::new(None),
    });
    let state = optional_capture_state(&harness, capture, "full_window");
    let mut units = vec![harness.make_units().remove(0)];
    let mut sibling = units[0].clone();
    sibling.row = sibling_row;
    units.push(sibling);
    assert_eq!(state.probe.enqueue_marker_refresh(units), 2);
    let first = state.probe.take_queued_for_test().unwrap();
    let second = state.probe.take_queued_for_test().unwrap();
    let sibling = if first.row.file.library_id.to_string() == sibling_id {
        first
    } else {
        second
    };
    let job_id = sibling.job_id.clone().unwrap();
    if target_completed {
        let target = harness.rows[0].file.file_id.to_string();
        harness
            .store
            .lock()
            .finish_probe_unit(&job_id, &target, true, None)
            .unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(patch_optional_capture(
        &state,
        &harness.library_id,
        "detect_intros",
    ));
    assert_eq!(
        state
            .probe
            .store
            .lock()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .status,
        if target_completed {
            "running"
        } else {
            "queued"
        },
        "the sibling's accepted work remains pending"
    );
    let pending = harness.store.lock().probe_job_units(&job_id).unwrap();
    assert!(pending.iter().any(|unit| {
        unit.ledger_id == sibling.row.file.file_id.to_string() && unit.status == "queued"
    }));
    state.probe.finish(&sibling, true);
    let job = state
        .probe
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.completed, 2);
    assert_eq!(job.succeeded, 1);
    harness.assert_old_markers_intact();
}

#[test]
fn disabled_owner_does_not_strand_or_publish_mixed_owner_manual_refresh() {
    mixed_owner_optional_cancellation(false);
}

#[test]
fn completed_target_is_cancelled_while_sibling_refresh_remains_pending() {
    mixed_owner_optional_cancellation(true);
}

#[test]
fn disabling_detection_cancels_completed_refresh_waiting_for_atomic_publication() {
    let harness = RefreshHarness::new();
    let capture = Arc::new(OptionalCapture {
        entered: Mutex::new(None),
        released: Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new())),
        stopped: Mutex::new(None),
    });
    let state = optional_capture_state(&harness, capture, "full_window");
    assert_eq!(state.probe.enqueue_marker_refresh(harness.make_units()), 2);
    let first = state.probe.take_queued_for_test().unwrap();
    let second = state.probe.take_queued_for_test().unwrap();
    let job_id = first.job_id.clone().unwrap();
    // The worker has finished all capture units; atomic marker publication has
    // not started. Persist the real completion receipts without calling publish.
    let completed = {
        let store = harness.store.lock();
        store
            .finish_probe_unit(&job_id, &first.row.file.file_id.to_string(), true, None)
            .unwrap();
        let (job, _, all_done) = store
            .finish_probe_unit(&job_id, &second.row.file.file_id.to_string(), true, None)
            .unwrap();
        assert!(all_done);
        assert_eq!(job.status, "running");
        job
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(patch_optional_capture(
        &state,
        &harness.library_id,
        "detect_intros",
    ));
    // Resume the already prepared worker after PATCH, using its prior receipt.
    super::super::marker_jobs::apply_marker_refresh(&state.probe, &completed, &second);
    let job = harness
        .store
        .lock()
        .get_probe_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        job.status, "cancelled",
        "completed capture still needs cancellation before atomic publication"
    );
    harness.assert_old_markers_intact();
}

struct PauseOptionalComparison {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl marker::FingerprintEngine for PauseOptionalComparison {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        Ok(vec![1, 2, 3, 4])
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        if first.is_empty() || second.is_empty() {
            return None;
        }
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.send(());
            let (released, changed) = self.released.as_ref();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut ready = released.lock().unwrap();
            while !*ready && std::time::Instant::now() < deadline {
                ready = changed
                    .wait_timeout(ready, std::time::Duration::from_millis(10))
                    .unwrap()
                    .0;
            }
        }
        Some(marker::CommonSegment {
            start1_sec: 10.0,
            end1_sec: 90.0,
            start2_sec: 10.0,
            end2_sec: 90.0,
            duration_sec: 80.0,
            score: 0.95,
        })
    }
}

#[test]
fn disabling_detection_during_comparison_prevents_season_model_publication() {
    let harness = RefreshHarness::new();
    let capture = Arc::new(OptionalCapture {
        entered: Mutex::new(None),
        released: Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new())),
        stopped: Mutex::new(None),
    });
    let mut state = optional_capture_state(&harness, capture.clone(), "adaptive");
    let (entered, comparing) = tokio::sync::oneshot::channel();
    let release = ReleaseOptionalCapture(Arc::new((
        std::sync::Mutex::new(false),
        std::sync::Condvar::new(),
    )));
    let matcher = Arc::new(PauseOptionalComparison {
        entered: Mutex::new(Some(entered)),
        released: release.0.clone(),
    });
    let manager = Arc::new(ProbeManager::with_engines(
        harness.store.clone(),
        matcher,
        capture,
    ).unwrap());
    manager.set_library_registry(state.library_work.clone());
    state.probe = manager.clone();
    assert_eq!(manager.enqueue_marker_refresh(harness.make_units()), 2);
    let units = vec![
        manager.take_queued_for_test().unwrap(),
        manager.take_queued_for_test().unwrap(),
    ];
    // Taking metadata messages in the fixture does not execute the worker's
    // MetadataStageGuard. Complete that handoff before entering its fingerprint
    // pipeline, otherwise MetadataPriorityGate correctly waits forever for us.
    for _ in &units {
        manager.metadata_stage_finished();
    }
    let job_id = units[0].job_id.clone().unwrap();
    let media_id = harness.media_id;
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::super::season::run_adaptive_season_pipeline(
                &manager, &job_id, media_id, 1, &units,
            ))
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), comparing)
            .await
            .unwrap()
            .unwrap();
        patch_optional_capture(&state, &harness.library_id, "detect_intros").await;
    });
    drop(release);
    assert!(
        worker.join().unwrap().is_err(),
        "cancelled season run must not succeed"
    );
    let models = harness
        .store
        .lock()
        .list_fingerprint_models(&harness.media_id.to_string(), 1)
        .unwrap();
    assert!(
        models.is_empty(),
        "a cancelled comparison must not leave a new durable season model"
    );
    harness.assert_old_markers_intact();
}
