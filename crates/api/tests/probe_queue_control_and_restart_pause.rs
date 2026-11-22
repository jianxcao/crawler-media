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

struct CountingTracks {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl CountingTracks {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(fail),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl api::probe_manager::TracksProbe for CountingTracks {
    fn probe(
        &self,
        _path: &Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(library::LibraryError::Probe("HTTP 403 Forbidden".into()));
        }
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_secs: Some(120.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

fn production_state(root: &Path, tracks: Arc<CountingTracks>) -> api::ApiState {
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
            true,
        )
        .unwrap()
    })
    .join()
    .unwrap();
    base.with_probe_engines(
        Arc::new(marker::ChromaprintEngine),
        Arc::new(marker::ChromaprintEngine),
        tracks,
    )
    .unwrap()
}

fn seed_scanned_movie(
    store: &api::Store,
    lib_root: &Path,
    title: &str,
    index: usize,
) -> (MediaId, LedgerId) {
    let path = lib_root.join(format!("{title}-{index}.mkv"));
    std::fs::write(&path, b"fixture-bytes").unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: format!("{title} {index}"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_restart_starts_paused_without_double_counting_and_resumes_on_command() {
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("domestic");
    let movie_root = dir.path().join("movies");
    std::fs::create_dir_all(&tv_root).unwrap();
    std::fs::create_dir_all(&movie_root).unwrap();
    {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "国产剧",
                &[tv_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "欧美电影",
                &[movie_root.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
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
        loop {
            let current = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
            if current["total_pending"] == 0 {
                break;
            }
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
        let lib_a = store
            .create_library(
                MediaKind::Movie,
                "国产剧",
                &[root_a.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "动漫",
                &[root_b.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
        let (m1, f1) = seed_scanned_movie(&store, &root_a, "a", 0);
        create_queued_job(&store, m1, f1, false);
        seed_scanned_movie(&store, &root_a, "a", 1);
        let (m2, f2) = seed_scanned_movie(&store, &root_b, "b", 2);
        create_queued_job(&store, m2, f2, false);
        seed_scanned_movie(&store, &root_b, "b", 3);
        lib_a.id
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
        let lib = store
            .create_library(
                MediaKind::Movie,
                "电影库",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let (m1, f1) = seed_scanned_movie(&store, &root, "single", 0);
        let (m2, f2) = seed_scanned_movie(&store, &root, "bulk", 1);
        (lib.id, m1, f1, m2, f2)
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
    tokio::time::timeout(Duration::from_secs(3), async {
        while state
            .store()
            .get_file_meta(&f1.to_string())
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("explicit single-item probe must complete even while background is paused");
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
        json!({
            "mode": "missing",
            "request_id": uuid::Uuid::new_v4().to_string(),
        }),
    )
    .await;
    assert_eq!(refresh["queued"], 1);
    assert!(!state.probe.background_is_paused());
    tokio::time::timeout(Duration::from_secs(3), async {
        while state
            .store()
            .get_file_meta(&f2.to_string())
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("explicit library media-info refresh must auto-resume and drain");
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
    store
        .create_library(
            MediaKind::Movie,
            "STRM",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
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
    tokio::time::timeout(Duration::from_secs(2), async {
        while store
            .active_probe_unit_for_ledger(&file_id.to_string())
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        manager
            .request_probe(&row, ProbeRequestOrigin::Detail)
            .unwrap(),
        ProbeRequestResult::Waiting {
            next_retry_at_ms: 1_060_000
        }
    );
    manager.shutdown_workers().await.unwrap();

    let restarted = Arc::new(
        ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(tracks.clone()),
    );
    restarted.set_clock(clock.clone());
    assert!(restarted.try_start_workers());
    assert_eq!(
        restarted
            .request_probe(&row, ProbeRequestOrigin::Detail)
            .unwrap(),
        ProbeRequestResult::Waiting {
            next_retry_at_ms: 1_060_000
        }
    );

    for deadline in [1_060_000, 1_360_000, 2_260_000, 5_860_000] {
        clock.set(deadline);
        assert_eq!(restarted.dispatch_due(deadline, 32).unwrap(), 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while store
                .active_probe_unit_for_ledger(&file_id.to_string())
                .unwrap()
                .is_some()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
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
