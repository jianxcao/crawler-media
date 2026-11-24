#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::ScannedLedger;
use tower::ServiceExt;

type FirstProbeGate = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

struct CountingTracks {
    calls: AtomicUsize,
    fail: AtomicBool,
    block_first: Mutex<Option<FirstProbeGate>>,
}

impl CountingTracks {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(fail),
            block_first: Mutex::new(None),
        })
    }

    fn blocking_first() -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let tracks = Self::new(false);
        let (entered_rx, release_tx) = tracks.arm_block_next();
        (tracks, entered_rx, release_tx)
    }

    fn arm_block_next(&self) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self.block_first.lock() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl marker::FingerprintEngine for CountingTracks {
    fn extract_at(
        &self,
        _: &Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        Ok(vec![11, 22, 33, 44])
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

impl marker::FingerprintCaptureEngine for CountingTracks {
    fn capture_window(
        &self,
        req: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Ok(marker::CapturedFingerprint {
            window: req.window.clone(),
            words: vec![11, 22, 33, 44],
            pcm_duration_ms: Some(req.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(req.window.start_ms),
                ..Default::default()
            },
        })
    }
}

impl api::probe_manager::TracksProbe for CountingTracks {
    fn probe(
        &self,
        _path: &Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((entered_tx, release_rx)) = self.block_first.lock().take() {
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(library::LibraryError::Probe("HTTP 403 Forbidden".into()));
        }
        let src = marker::CaptureSourceFacts {
            audio_stream_index: 0,
            format_start_time_us: Some(0),
            stream_start_pts: Some(0),
            stream_time_base: Some("1/16000".into()),
            codec_name: None,
            native_sample_rate: None,
            audio_channels: None,
            provenance: "fixture".into(),
        };
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_secs: Some(1_200.0),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    is_default: true,
                    capture_source: Some(src),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(1_200_000),
        ))
    }
}

fn production_state(root: &Path, tracks: Arc<CountingTracks>) -> api::ApiState {
    production_state_with_writable(root, tracks, true)
}

fn production_state_with_writable(
    root: &Path,
    tracks: Arc<CountingTracks>,
    writable: bool,
) -> api::ApiState {
    let root = root.to_path_buf();
    let base = std::thread::spawn(move || {
        let store = Arc::new(api::Store::open(root.join("data")).unwrap());
        let action = api::bootstrap_credentials::prepare_admin_credentials(
            store.as_ref(),
            "management-secret",
            None,
            api::bootstrap_credentials::BootstrapMode::TestFixture,
        )
        .unwrap();
        api::ApiState::new_production_with_admin_bootstrap_writable(
            store,
            ProfileSet::load(None).unwrap(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(root.join("dl"))),
            action,
            writable,
        )
        .unwrap()
    })
    .join()
    .unwrap();
    base.with_probe_engines(tracks.clone(), tracks.clone(), tracks)
        .unwrap()
}

fn insert_test_media(store: &api::Store, kind: MediaKind, title: String) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind,
            title,
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn seed_scanned_movie(
    store: &api::Store,
    lib_root: &Path,
    title: &str,
    index: usize,
) -> (MediaId, LedgerId) {
    let path = lib_root.join(format!("{title}-{index}.mkv"));
    std::fs::write(&path, b"fixture-bytes").unwrap();
    let media_id = insert_test_media(store, MediaKind::Movie, format!("{title} {index}"));
    let file_id = LedgerId::new();
    let source_version = api::fingerprint_job::current_source_version(&path);
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id,
                media_id,
                path: path.display().to_string(),
                units: vec![UnitKey::WHOLE],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version,
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            index as i64 + 1,
        )
        .unwrap();
    (media_id, file_id)
}

fn create_queued_job(store: &api::Store, media_id: MediaId, file_id: LedgerId, running: bool) {
    let job_id = uuid::Uuid::new_v4().to_string();
    let ledger = file_id.to_string();
    assert!(
        store
            .create_probe_job(
                &job_id,
                "media_probe",
                &media_id.to_string(),
                None,
                &format!("ledger:{ledger}"),
                &[store::ProbeJobUnitSpec {
                    ledger_id: &ledger,
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
    if running {
        assert!(
            store
                .start_probe_unit(&job_id, &ledger, store::ProbeDispatchStage::Metadata)
                .unwrap()
        );
    }
}

async fn admin_json(app: &axum::Router, method: &str, uri: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(common::request(
            method,
            uri,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
    common::json_data(response).await
}

fn create_lib(
    store: &api::Store,
    kind: MediaKind,
    name: &str,
    root: &Path,
    auto_probe: bool,
) -> String {
    store
        .create_library(
            kind,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            auto_probe,
            &[],
        )
        .unwrap()
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_restart_starts_paused_without_double_counting_and_resumes_on_command() {
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("domestic");
    let movie_root = dir.path().join("movies");
    std::fs::create_dir_all(&tv_root).unwrap();
    std::fs::create_dir_all(&movie_root).unwrap();
    {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        create_lib(&store, MediaKind::Movie, "国产剧", &tv_root, true);
        create_lib(&store, MediaKind::Movie, "欧美电影", &movie_root, false);
        let (m1, f1) = seed_scanned_movie(&store, &tv_root, "overlap", 0);
        // f1 has BOTH an un-acked scan obligation AND an interrupted running unit.
        create_queued_job(&store, m1, f1, true);
        seed_scanned_movie(&store, &tv_root, "ob-only", 1);
        seed_scanned_movie(&store, &movie_root, "west", 2);
    }

    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks.clone());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let (_, member_token) = common::create_member(&app, "viewer").await;
    for (method, uri) in [
        ("GET", "/api/v1/probe/status"),
        ("POST", "/api/v1/probe/pause"),
        ("POST", "/api/v1/probe/resume"),
        ("POST", "/api/v1/probe/cancel"),
    ] {
        let resp = app
            .clone()
            .oneshot(common::request(method, uri, Some(&member_token), json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }

    let status = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
    assert_eq!(status["paused"], true);
    assert_eq!(status["queued_metadata"], 1);
    assert_eq!(status["running_metadata"], 0);
    assert_eq!(status["pending_obligations"], 2);
    assert_eq!(status["total_pending"], 3);
    assert_eq!(status["libraries"].as_array().unwrap().len(), 2);

    assert_eq!(
        api::scan_probe_dispatch::dispatch_once(state.clone())
            .await
            .unwrap(),
        0
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        tracks.calls(),
        0,
        "startup pause must block all background probes"
    );

    let resumed = admin_json(&app, "POST", "/api/v1/probe/resume", json!({})).await;
    assert_eq!(resumed["paused"], false);

    tokio::time::timeout(Duration::from_secs(5), async {
        while admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await["total_pending"]
            != 0
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("resumed background queue must drain all obligations and queued units");
    assert_eq!(tracks.calls(), 3);
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_endpoint_clears_library_or_global_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("lib-a");
    let root_b = dir.path().join("lib-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let lib_a_id = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let lib_a_id = create_lib(&store, MediaKind::Movie, "国产剧", &root_a, true);
        create_lib(&store, MediaKind::Movie, "动漫", &root_b, false);
        let (m1, f1) = seed_scanned_movie(&store, &root_a, "a", 0);
        create_queued_job(&store, m1, f1, false);
        seed_scanned_movie(&store, &root_a, "a", 1);
        let (m2, f2) = seed_scanned_movie(&store, &root_b, "b", 2);
        create_queued_job(&store, m2, f2, false);
        seed_scanned_movie(&store, &root_b, "b", 3);
        lib_a_id
    };

    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks);
    let app = api::router(state.clone());

    let receipt_a = admin_json(
        &app,
        "POST",
        "/api/v1/probe/cancel",
        json!({ "library_id": lib_a_id }),
    )
    .await;
    assert_eq!(receipt_a["cancelled_units"], 1);
    assert_eq!(receipt_a["cleared_obligations"], 2);

    let after_a = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
    assert_eq!(after_a["total_pending"], 2);
    assert_eq!(after_a["libraries"][0]["library_name"], "动漫");

    let empty_body_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/probe/cancel")
                .header("authorization", "Bearer management-secret")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty_body_resp.status(), StatusCode::OK);
    let receipt_all = common::json_data(empty_body_resp).await;
    assert_eq!(receipt_all["cancelled_units"], 1);
    assert_eq!(receipt_all["cleared_obligations"], 2);

    let after_all = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
    assert_eq!(after_all["total_pending"], 0);
    assert!(after_all["libraries"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_item_probe_runs_while_paused_and_library_media_info_refresh_auto_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let (lib_id, m1, f1, _m2, f2) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let lib_id = create_lib(&store, MediaKind::Movie, "电影库", &root, true);
        let (m1, f1) = seed_scanned_movie(&store, &root, "single", 0);
        let (m2, f2) = seed_scanned_movie(&store, &root, "bulk", 1);
        (lib_id, m1, f1, m2, f2)
    };

    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks.clone());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let single = admin_json(
        &app,
        "POST",
        &format!("/api/v1/libraries/{lib_id}/items/{m1}/probe"),
        json!({}),
    )
    .await;
    assert_eq!(single["queued"], 1);
    poll_until(|| {
        state
            .store()
            .get_file_meta(&f1.to_string())
            .unwrap()
            .is_some()
    })
    .await;
    assert!(state.probe.background_is_paused());
    assert!(
        state
            .store()
            .get_file_meta(&f2.to_string())
            .unwrap()
            .is_none()
    );

    let refresh = admin_json(
        &app,
        "POST",
        &format!("/api/v1/libraries/{lib_id}/media-info/refresh"),
        json!({ "mode": "missing", "request_id": uuid::Uuid::new_v4().to_string() }),
    )
    .await;
    assert_eq!(refresh["queued"], 1);
    assert!(!state.probe.background_is_paused());
    poll_until(|| {
        state
            .store()
            .get_file_meta(&f2.to_string())
            .unwrap()
            .is_some()
    })
    .await;
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_ffprobe_failure_persists_backoff_across_restarts_until_exhausted() {
    use api::probe_manager::ProbeManager;
    use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("strm");
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    create_lib(&store, MediaKind::Movie, "STRM", &root, true);
    let (_media_id, file_id) = seed_scanned_movie(&store, &root, "broken", 0);
    let row = store.file_coverage_by_id(file_id).unwrap().unwrap();
    let tracks = CountingTracks::new(true);
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000_000));

    let manager = Arc::new(
        ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(tracks.clone()),
    );
    manager.set_clock(clock.clone());
    assert!(manager.try_start_workers());

    assert!(matches!(
        manager
            .request_probe(&row, ProbeRequestOrigin::Detail)
            .unwrap(),
        ProbeRequestResult::Queued { .. }
    ));
    poll_until(|| {
        store
            .active_probe_unit_for_ledger(&file_id.to_string())
            .unwrap()
            .is_none()
    })
    .await;
    let waiting = manager
        .request_probe(&row, ProbeRequestOrigin::Detail)
        .unwrap();
    assert!(
        matches!(
            waiting,
            ProbeRequestResult::Waiting { next_retry_at_ms }
                if (1_055_000..=1_060_500).contains(&next_retry_at_ms)
        ),
        "unexpected waiting state: {waiting:?}"
    );
    manager.shutdown_workers().await.unwrap();

    let restarted = Arc::new(
        ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(tracks.clone()),
    );
    restarted.set_clock(clock.clone());
    assert!(restarted.try_start_workers());
    // Restart drops in-memory failure cooldown while preserving durable failure_count (1/5).
    assert!(matches!(
        restarted
            .request_probe(&row, ProbeRequestOrigin::Detail)
            .unwrap(),
        ProbeRequestResult::Queued { .. }
    ));
    poll_until(|| {
        store
            .active_probe_unit_for_ledger(&file_id.to_string())
            .unwrap()
            .is_none()
    })
    .await;

    for deadline in [1_360_000, 2_260_000, 5_860_000] {
        clock.set(deadline);
        assert_eq!(restarted.dispatch_due(deadline, 32).unwrap(), 1);
        poll_until(|| {
            store
                .active_probe_unit_for_ledger(&file_id.to_string())
                .unwrap()
                .is_none()
        })
        .await;
    }
    clock.set(i64::MAX / 4);
    assert_eq!(restarted.dispatch_due(i64::MAX / 4, 32).unwrap(), 0);
    assert_eq!(
        restarted
            .request_probe(&row, ProbeRequestOrigin::Detail)
            .unwrap(),
        ProbeRequestResult::Exhausted
    );
    assert_eq!(tracks.calls(), 5);
    restarted.shutdown_workers().await.unwrap();
}

fn seed_tv_episodes(store: &api::Store, tv_root: &Path, count: u32) -> (MediaId, Vec<LedgerId>) {
    use store::test_support::{FixtureFile, publish_fixture_file};
    let lib_id = create_lib(store, MediaKind::Tv, "TV", tv_root, true);
    store
        .set_library_intro_settings(&lib_id, true, true)
        .unwrap();
    let show = insert_test_media(store, MediaKind::Tv, "Paused Show".into());
    let ids = (1..=count)
        .map(|ep| {
            let p = tv_root.join(format!("S01E0{ep}.mkv"));
            std::fs::write(&p, b"tv-bytes").unwrap();
            let spec = FixtureFile {
                media_id: show,
                path: p.display().to_string(),
                season: Some(1),
                episode: Some(ep),
                ..Default::default()
            };
            publish_fixture_file(store, &spec).unwrap().file_id
        })
        .collect();
    (show, ids)
}

async fn poll_until(mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();
}

fn has_meta(state: &api::ApiState, id: LedgerId) -> bool {
    state
        .store()
        .get_file_meta(&id.to_string())
        .unwrap()
        .is_some()
}

fn in_analysis(state: &api::ApiState, id: LedgerId) -> bool {
    state
        .store()
        .active_probe_unit_for_ledger(&id.to_string())
        .unwrap()
        .is_some_and(|u| u.stage.as_str() == "analysis")
}

#[path = "probe_queue_control_and_restart_pause/paused_analysis.rs"]
mod paused_analysis;
#[path = "probe_queue_control_and_restart_pause/remediation_cases.rs"]
mod remediation_cases;
#[path = "probe_queue_control_and_restart_pause/round3_remediation.rs"]
mod round3_remediation;
