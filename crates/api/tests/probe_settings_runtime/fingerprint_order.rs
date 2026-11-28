use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const FP_SETTINGS: &str = "/api/v1/settings/probe/fingerprint";
const FP_STATUS: &str = "/api/v1/probe/fingerprint/status";

async fn fp_req(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(common::request(
            method,
            path,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    let status = resp.status();
    let data = common::json_data(resp).await;
    (status, data)
}

struct GateFingerprint {
    active: AtomicUsize,
    max_active: AtomicUsize,
    release: AtomicBool,
    started_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
}

impl GateFingerprint {
    fn new() -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                release: AtomicBool::new(false),
                started_tx: Mutex::new(Some(tx)),
            }),
            rx,
        )
    }
}

impl marker::FingerprintEngine for GateFingerprint {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        Ok((0..300).map(|i| i as u32 + 1).collect())
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

impl marker::FingerprintCaptureEngine for GateFingerprint {
    fn capture_window_controlled(
        &self,
        request: &marker::CaptureRequest,
        control: &marker::WorkControl,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        if let Some(tx) = self.started_tx.lock().as_ref() {
            let _ = tx.send(request.path.display().to_string());
        }
        while !self.release.load(Ordering::Acquire) {
            if control.is_cancelled() {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return Err(marker::CaptureFailure {
                    kind: marker::CaptureFailureKind::Cancelled,
                    message: "cancelled".into(),
                    metrics: marker::CaptureMetrics::default(),
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        let dur_ms = request.window.duration_ms();
        let num_words = (dur_ms / 120).max(32) as usize;
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words: (0..num_words).map(|i| i as u32 + 1).collect(),
            pcm_duration_ms: Some(dur_ms),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                pcm_frame_ms: 20,
                audio_stream_index: Some(0),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                elapsed_ms: 1,
                pcm_bytes: (num_words * 4) as u64,
                ..Default::default()
            },
        })
    }

    fn capture_window_from_source_controlled(
        &self,
        request: &marker::CaptureRequest,
        _source: &marker::CaptureSourceFacts,
        control: &marker::WorkControl,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.capture_window_controlled(request, control)
    }

    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.capture_window_controlled(request, &marker::WorkControl::default())
    }
}

fn seed_tv_episodes_with_cached_meta(
    store: &api::Store,
    root: &Path,
    episodes: &[u32],
) -> (String, domain::MediaId, Vec<domain::LedgerId>) {
    let tv_root = root.join("tv_order");
    std::fs::create_dir_all(&tv_root).unwrap();
    let lib = store
        .create_library(
            domain::MediaKind::Tv,
            "TV Order",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Tv,
            title: "Order Test".into(),
            year: Some(2025),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let mut ids = Vec::new();
    for &ep in episodes {
        let path = tv_root.join(format!("Order.S01E{ep:02}.mkv"));
        std::fs::write(&path, format!("ep-{ep}")).unwrap();
        let fid = domain::LedgerId::new();
        let sv = api::fingerprint_job::current_source_version(&path);
        store
            .insert_scanned_files(
                &[store::ScannedLedger {
                    file_id: fid,
                    media_id,
                    path: path.to_string_lossy().into_owned(),
                    units: vec![domain::UnitKey::new(1, ep as i32)],
                    resolution: Some("1080p".into()),
                    codec: None,
                    hdr: None,
                    quality_source: domain::QualitySource::Release,
                    confidence: domain::Confidence::High,
                    filter_score: None,
                    source_version: sv.clone(),
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: Vec::new(),
                }],
                1,
            )
            .unwrap();
        store
            .put_file_meta_versioned(
                &fid.to_string(),
                &library::Tracks {
                    video: Some(Default::default()),
                    ..Default::default()
                },
                Some(&sv),
                Some(1_500_000),
            )
            .unwrap();
        ids.push(fid);
    }
    (lib.id, media_id, ids)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fingerprint_settings_late_put_does_not_overwrite_newer_pause_and_worker_limit() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let st = state(root.path()).with_fingerprint_settings_after_write_barrier(Arc::new(
        move |workers, paused| {
            if workers == 4 && !paused {
                if let Some(tx) = entered.lock().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().take() {
                    let _ = rx.recv_timeout(Duration::from_secs(5));
                }
            }
        },
    ));
    let app = api::router(st.clone());

    let app_a = app.clone();
    let req_a = tokio::spawn(async move {
        fp_req(
            &app_a,
            "PUT",
            FP_SETTINGS,
            json!({"workers": 4, "paused": false}),
        )
        .await
    });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("request A must reach post-write barrier");

    let (st_b, _) = fp_req(
        &app,
        "PUT",
        FP_SETTINGS,
        json!({"workers": 1, "paused": true}),
    )
    .await;
    assert_eq!(st_b, StatusCode::OK);

    release_tx.send(()).unwrap();
    let (st_a, _) = req_a.await.unwrap();
    assert_eq!(st_a, StatusCode::OK);

    let (_, saved) = fp_req(&app, "GET", FP_SETTINGS, Value::Null).await;
    let (_, status) = fp_req(&app, "GET", FP_STATUS, Value::Null).await;
    assert_eq!(saved["workers"], status["workers"]);
    assert_eq!(saved["paused"], status["paused"]);
    assert_eq!(status["workers"], 1);
    assert_eq!(status["paused"], true);
    st.probe.shutdown_workers().await.unwrap();

    // Restarted ApiState must read back the exact same persisted settings.
    let restarted = state(root.path());
    let restarted_app = api::router(restarted.clone());
    let (_, after_restart) = fp_req(&restarted_app, "GET", FP_SETTINGS, Value::Null).await;
    let (_, restart_status) = fp_req(&restarted_app, "GET", FP_STATUS, Value::Null).await;
    assert_eq!(after_restart, json!({"workers": 1, "paused": true}));
    assert_eq!(restart_status["workers"], 1);
    assert_eq!(restart_status["paused"], true);
    restarted.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fingerprint_settings_late_pause_does_not_drain_or_clobber_newer_resume() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let (fp_engine, mut started_rx) = GateFingerprint::new();
    fp_engine.release.store(true, Ordering::Release);

    let base = state(root.path());
    base.probe.shutdown_workers().await.unwrap();
    let st = base
        .with_probe_engines(
            fp_engine.clone(),
            fp_engine.clone(),
            Arc::new(HeldTracks(tokio::sync::mpsc::unbounded_channel().0)),
        )
        .unwrap()
        .with_fingerprint_settings_after_write_barrier(Arc::new(move |_workers, paused| {
            if paused {
                if let Some(tx) = entered.lock().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().take() {
                    let _ = rx.recv_timeout(Duration::from_secs(5));
                }
            }
        }));
    assert!(st.probe.try_start_workers());
    let app = api::router(st.clone());

    let (_lib, media_id, fids) = seed_tv_episodes_with_cached_meta(&st.store(), root.path(), &[1]);
    let app_a = app.clone();
    let req_pause = tokio::spawn(async move {
        fp_req(&app_a, "POST", "/api/v1/probe/fingerprint/pause", json!({})).await
    });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("pause request must reach post-write barrier");

    // Enqueue a fingerprint job while paused, then resume before the older pause returns.
    assert!(
        st.store()
            .create_probe_job(
                "fp-after-resume",
                "fingerprint_probe",
                &media_id.to_string(),
                Some(1),
                &format!("ledger:{}", fids[0]),
                &[store::ProbeJobUnitSpec {
                    ledger_id: &fids[0].to_string(),
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: true,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );

    let (st_res, _) = fp_req(&app, "POST", "/api/v1/probe/fingerprint/resume", json!({})).await;
    assert_eq!(st_res, StatusCode::OK);
    release_tx.send(()).unwrap();
    let (st_p, _) = req_pause.await.unwrap();
    assert_eq!(st_p, StatusCode::OK);

    let (_, saved) = fp_req(&app, "GET", FP_SETTINGS, Value::Null).await;
    let (_, status) = fp_req(&app, "GET", FP_STATUS, Value::Null).await;
    assert_eq!(saved["paused"], false);
    assert_eq!(status["paused"], false);

    // The queued fingerprint job must actually run and complete after resume.
    tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
        .await
        .expect("resumed fingerprint work must start")
        .unwrap();
    st.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fingerprint_worker_decrease_and_paused_explicit_detect_exemption() {
    let _serial = PROCESS_GATE.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (fp_engine, mut started_rx) = GateFingerprint::new();
    let base = state(root.path());
    base.probe.shutdown_workers().await.unwrap();
    let st = base
        .with_probe_engines(
            fp_engine.clone(),
            fp_engine.clone(),
            Arc::new(HeldTracks(tokio::sync::mpsc::unbounded_channel().0)),
        )
        .unwrap();
    assert!(st.probe.try_start_workers());
    let app = api::router(st.clone());

    let (st_put, _) = fp_req(
        &app,
        "PUT",
        FP_SETTINGS,
        json!({"workers": 2, "paused": false}),
    )
    .await;
    assert_eq!(st_put, StatusCode::OK);

    let (lib_id, media_id, fids) =
        seed_tv_episodes_with_cached_meta(&st.store(), root.path(), &[1, 2, 3, 4]);
    for (idx, fid) in fids[..3].iter().enumerate() {
        let jid = format!("fp-lim-{idx}");
        assert!(
            st.store()
                .create_probe_job(
                    &jid,
                    "fingerprint_probe",
                    &media_id.to_string(),
                    Some(1),
                    &format!("ledger:{fid}"),
                    &[store::ProbeJobUnitSpec {
                        ledger_id: &fid.to_string(),
                        kind: "tv",
                        force_fingerprint: true,
                        reuse_fingerprint_cache: false,
                        overwrite_markers: true,
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap()
        );
    }
    st.probe.wake_dispatch();

    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("two workers must start")
            .unwrap();
    }
    assert_eq!(fp_engine.active.load(Ordering::SeqCst), 2);

    // Decreasing workers to 1 must NOT interrupt active 2, and 3rd must not start while 2 active.
    let (st_dec, _) = fp_req(
        &app,
        "PUT",
        FP_SETTINGS,
        json!({"workers": 1, "paused": false}),
    )
    .await;
    assert_eq!(st_dec, StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_millis(120), started_rx.recv())
            .await
            .is_err(),
        "3rd unit must wait while active >= new limit 1"
    );
    fp_engine.release.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
        .await
        .expect("3rd unit must run once active drops below 1")
        .unwrap();

    // Pause queue and verify explicit single-episode detect still runs as pause-exempt.
    let (st_pause, _) = fp_req(&app, "POST", "/api/v1/probe/fingerprint/pause", json!({})).await;
    assert_eq!(st_pause, StatusCode::OK);
    let (st_det, _) = fp_req(
        &app,
        "POST",
        &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers/detect"),
        json!({"season": 1, "episode": 4, "file_id": fids[3].to_string(), "mode": "recapture"}),
    )
    .await;
    assert_eq!(st_det, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
        .await
        .expect("explicit detect must remain pause-exempt")
        .unwrap();
    st.probe.shutdown_workers().await.unwrap();
}
