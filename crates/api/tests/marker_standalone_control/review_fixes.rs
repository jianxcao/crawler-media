//! TDD regression coverage for manual unlock, duration bounds, Movie markers,
//! targeted `file_id` cancellation without `probe_stage_state` wedging, and `theintrodb_only`.

#[path = "review_fixes/cloud_sync.rs"]
mod cloud_sync;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::ScannedLedger;
use tower::ServiceExt;

use super::{ControllableFingerprint, common, seed_tv_library_and_episodes, test_state};

pub(super) struct PanicChapterProbe;
impl api::marker_resolver::ChapterProbe for PanicChapterProbe {
    fn probe(&self, _path: &Path) -> Option<Vec<marker::Chapter>> {
        panic!("theintrodb_only must never invoke synchronous local ffprobe chapter probing");
    }
}

pub(super) struct ScriptedHttpResponse {
    pub(super) status_line: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: String,
}

pub(super) struct StubIntroDbServer {
    pub(super) url: String,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    queue: Arc<Mutex<VecDeque<ScriptedHttpResponse>>>,
}

impl StubIntroDbServer {
    pub(super) fn start() -> Self {
        Self::start_with_queue(Arc::new(Mutex::new(VecDeque::new())))
    }

    pub(super) fn start_scripted(responses: Vec<ScriptedHttpResponse>) -> Self {
        Self::start_with_queue(Arc::new(Mutex::new(VecDeque::from(responses))))
    }

    pub(super) fn push_response(&self, resp: ScriptedHttpResponse) {
        self.queue.lock().push_back(resp);
    }

    fn start_with_queue(queue: Arc<Mutex<VecDeque<ScriptedHttpResponse>>>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let queue_c = queue.clone();
        let handle = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                if let Some(scripted) = queue_c.lock().pop_front() {
                    let mut extra_headers = String::new();
                    for (k, v) in scripted.headers {
                        extra_headers.push_str(&format!("{k}: {v}\r\n"));
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        scripted.status_line,
                        extra_headers,
                        scripted.body.len(),
                        scripted.body
                    );
                    continue;
                }
                let first_line = req.lines().next().unwrap_or("");
                let query = first_line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.split_once('?').map(|(_, q)| q))
                    .unwrap_or("");
                let param = |key: &str| -> Option<String> {
                    query.split('&').find_map(|kv| {
                        let (k, v) = kv.split_once('=')?;
                        (k == key).then(|| v.to_string())
                    })
                };
                let body = if let (Some(s), Some(e)) = (param("season"), param("episode")) {
                    format!(
                        r#"{{"type":"tv","season":{s},"episode":{e},"group_duration_ms":1500000,"intro":[{{"start_ms":12000,"end_ms":42000}}],"credits":[{{"start_ms":1420000,"end_ms":1490000}}]}}"#
                    )
                } else {
                    r#"{"type":"movie","season":null,"episode":null,"group_duration_ms":7200000,"intro":[{"start_ms":0,"end_ms":90000}],"credits":[{"start_ms":6900000,"end_ms":7180000}]}"#.to_string()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        Self {
            url,
            stop,
            handle: Some(handle),
            queue,
        }
    }
}

impl Drop for StubIntroDbServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub(super) struct StubIntroDbFactory(pub(super) String);
impl api::theintrodb_lookup::ClientFactory for StubIntroDbFactory {
    fn create(&self, api_key: Option<String>) -> api::theintrodb::TheIntroDbClient {
        api::theintrodb::TheIntroDbClient::new(api_key).with_base_url(&self.0)
    }
}

pub(super) fn seed_movie_with_duration(
    store: &api::Store,
    root: &Path,
    duration_ms: i64,
) -> (String, MediaId, LedgerId) {
    let movie_root = root.join("movies");
    std::fs::create_dir_all(&movie_root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[movie_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Dune".into(),
            year: Some(2021),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("438631".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = movie_root.join("Dune.2021.mkv");
    std::fs::write(&path, b"movie-bytes").unwrap();
    let fid = LedgerId::new();
    let sv = api::fingerprint_job::current_source_version(&path);
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id: fid,
                media_id,
                path: path.to_string_lossy().into_owned(),
                units: vec![UnitKey::WHOLE],
                resolution: Some("2160p".into()),
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version: sv,
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
    let file = store.file_coverage_by_id(fid).unwrap().unwrap().file;
    store
        .publish_probe_result(
            fid,
            &store::library_write::probe::ProbePublication {
                expected: &file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: Some(duration_ms),
                details: None,
            },
            0,
        )
        .unwrap();
    (lib.id, media_id, fid)
}

pub(super) fn publish_duration(store: &api::Store, fid: LedgerId, duration_ms: i64) {
    let file = store.file_coverage_by_id(fid).unwrap().unwrap().file;
    store
        .publish_probe_result(
            fid,
            &store::library_write::probe::ProbePublication {
                expected: &file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: Some(duration_ms),
                details: None,
            },
            0,
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_marker_unlock_duration_overflow_and_strict_json_contract() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_id, media_id, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1], false);
    publish_duration(&state.store(), ep_ids[0], 1_500_000);
    let url = format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers");

    let unknown_field = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &url,
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 5_000,
                "intro_end_ms": 30_000,
                "locked": true,
                "obsolete": 1
            }),
        ))
        .await
        .unwrap();
    assert_eq!(unknown_field.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_body(unknown_field).await["error"]["code"],
        "request.invalid"
    );

    let overflow = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &url,
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "outro_start_ms": 1_400_000,
                "outro_end_ms": 1_600_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(overflow.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_body(overflow).await["error"]["code"],
        "markers.invalid"
    );

    let locked_res = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &url,
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 10_000,
                "intro_end_ms": 40_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(locked_res.status(), StatusCode::OK);
    assert_eq!(common::json_data(locked_res).await["locked"], true);

    let unlocked_res = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &url,
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 12_500,
                "intro_end_ms": 42_500,
                "locked": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(unlocked_res.status(), StatusCode::OK);
    let unlocked = common::json_data(unlocked_res).await;
    assert_eq!(unlocked["locked"], false);
    assert_eq!(unlocked["intro_start_ms"], 12_500);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn movie_manual_marker_crud_and_diagnostics_contract() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_id, media_id, fid) = seed_movie_with_duration(&state.store(), dir.path(), 7_200_000);
    let base = format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers");

    let unsupported_detect = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("{base}/detect"),
            Some("management-secret"),
            json!({ "mode": "recapture" }),
        ))
        .await
        .unwrap();
    assert_eq!(unsupported_detect.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_body(unsupported_detect).await["error"]["code"],
        "markers.unsupported_kind"
    );

    let put_res = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &base,
            Some("management-secret"),
            json!({
                "file_id": fid.to_string(),
                "intro_start_ms": 0,
                "intro_end_ms": 85_000,
                "outro_start_ms": 6_900_000,
                "outro_end_ms": 7_190_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put_res.status(), StatusCode::OK);

    let get_res = app
        .clone()
        .oneshot(common::request(
            "GET",
            &base,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);
    let diag = common::json_data(get_res).await;
    assert_eq!(diag["media_id"], media_id.to_string());
    assert!(diag["season"].is_null());
    assert_eq!(diag["paused"], false);
    assert_eq!(diag["episodes"][0]["file_id"], fid.to_string());
    assert!(diag["episodes"][0]["season"].is_null());
    assert!(diag["episodes"][0]["episode"].is_null());
    assert_eq!(diag["episodes"][0]["marker"]["intro_end_ms"], 85_000);

    let del_res = app
        .oneshot(common::request(
            "DELETE",
            &format!("{base}?file_id={fid}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::OK);
    assert!(
        state
            .store()
            .get_file_marker(fid, UnitKey::WHOLE)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_id_cancel_clears_probe_stage_state_allowing_immediate_redetect() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    state.probe.try_start_workers();
    let app = api::router(state.clone());

    let (lib_id, media_id, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1, 2], false);
    for &fid in &ep_ids {
        let cov = state.store().file_coverage_by_id(fid).unwrap().unwrap();
        state
            .probe
            .request_probe(&cov, api::probe_manager::policy::ProbeRequestOrigin::Detail)
            .unwrap();
    }
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("first capture started");

    let target_fid = ep_ids[0].to_string();
    let cancel_res = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/cancel",
            Some("management-secret"),
            json!({ "file_id": target_fid, "include_running": true }),
        ))
        .await
        .unwrap();
    assert_eq!(cancel_res.status(), StatusCode::OK);
    let receipt = common::json_data(cancel_res).await;
    assert_eq!(receipt["cancelled_units"], 1);
    assert_eq!(receipt["aborted_running"], 1);

    fp.block_capture.store(false, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let a0 = state
            .store()
            .active_probe_unit_for_ledger(&ep_ids[0].to_string())
            .unwrap();
        let a1 = state
            .store()
            .active_probe_unit_for_ledger(&ep_ids[1].to_string())
            .unwrap();
        if a0.is_none() && a1.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "units did not settle");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let redetect = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers/detect"),
            Some("management-secret"),
            json!({ "season": 1, "episode": 1, "file_id": ep_ids[0].to_string(), "mode": "recapture" }),
        ))
        .await
        .unwrap();
    assert_eq!(redetect.status(), StatusCode::OK);
    assert_eq!(common::json_data(redetect).await["queued"], true);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let active = state
            .store()
            .active_probe_unit_for_ledger(&ep_ids[0].to_string())
            .unwrap();
        let unit = state
            .store()
            .latest_probe_unit_for_ledger(&ep_ids[0].to_string())
            .unwrap()
            .unwrap();
        if active.is_none() && unit.status == "succeeded" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "redetect wedged after cancel: {unit:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn theintrodb_only_tv_rejects_disabled_skips_ffprobe_and_respects_locks() {
    let dir = tempfile::tempdir().unwrap();
    let server = StubIntroDbServer::start();
    let state = test_state(dir.path(), ControllableFingerprint::fast())
        .with_chapter_probe(Arc::new(PanicChapterProbe))
        .with_theintrodb_factory(Arc::new(StubIntroDbFactory(server.url.clone())));
    let app = api::router(state.clone());

    let (tv_lib, tv_media, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1, 2], false);
    state
        .store()
        .set_library_intro_settings(&tv_lib, false, false)
        .unwrap();
    for &fid in &ep_ids {
        publish_duration(&state.store(), fid, 1_500_000);
    }

    let disabled = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{tv_lib}/items/{tv_media}/markers/detect"),
            Some("management-secret"),
            json!({ "season": 1, "mode": "theintrodb_only" }),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_body(disabled).await["error"]["code"],
        "markers.theintrodb_disabled"
    );

    state
        .store()
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true}"#)
        .unwrap();
    let _ = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &format!("/api/v1/libraries/{tv_lib}/items/{tv_media}/markers"),
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 5_000,
                "intro_end_ms": 25_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();

    let tv_res = app
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{tv_lib}/items/{tv_media}/markers/detect"),
            Some("management-secret"),
            json!({ "season": 1, "mode": "theintrodb_only" }),
        ))
        .await
        .unwrap();
    assert_eq!(tv_res.status(), StatusCode::OK);
    assert_eq!(common::json_data(tv_res).await["updated_episodes"], 1);
    assert_eq!(
        state
            .store()
            .get_file_marker(ep_ids[0], UnitKey::new(1, 1))
            .unwrap()
            .unwrap()
            .source,
        "manual"
    );
    assert_eq!(
        state
            .store()
            .get_file_marker(ep_ids[1], UnitKey::new(1, 2))
            .unwrap()
            .unwrap()
            .source,
        "theintrodb"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn theintrodb_only_movie_syncs_cloud_markers_without_ffprobe() {
    let dir = tempfile::tempdir().unwrap();
    let server = StubIntroDbServer::start();
    let state = test_state(dir.path(), ControllableFingerprint::fast())
        .with_chapter_probe(Arc::new(PanicChapterProbe))
        .with_theintrodb_factory(Arc::new(StubIntroDbFactory(server.url.clone())));
    state
        .store()
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true}"#)
        .unwrap();
    let app = api::router(state.clone());

    let (mv_lib, mv_media, mv_fid) =
        seed_movie_with_duration(&state.store(), dir.path(), 7_200_000);
    let mv_res = app
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{mv_lib}/items/{mv_media}/markers/detect"),
            Some("management-secret"),
            json!({ "mode": "theintrodb_only" }),
        ))
        .await
        .unwrap();
    assert_eq!(mv_res.status(), StatusCode::OK);
    assert_eq!(common::json_data(mv_res).await["updated_episodes"], 1);
    assert_eq!(
        state
            .store()
            .get_file_marker(mv_fid, UnitKey::WHOLE)
            .unwrap()
            .unwrap()
            .source,
        "theintrodb"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_resume_preserves_manual_marker_exemption_while_fingerprint_paused() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    state.probe.pause_background();
    let app = api::router(state.clone());

    let _ = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/pause",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();

    let (lib_id, media_id, _ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1, 2, 3], false);
    let detect = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers/detect"),
            Some("management-secret"),
            json!({ "season": 1, "mode": "recapture" }),
        ))
        .await
        .unwrap();
    assert_eq!(detect.status(), StatusCode::OK);

    let bg_resume = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/resume",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(bg_resume.status(), StatusCode::OK);
    assert!(state.probe.fingerprint_is_paused());
    assert!(
        state.probe.has_paused_exempt_work(),
        "resuming background probe while fingerprint queue stays paused must keep manual marker exemption"
    );

    state.probe.try_start_workers();
    let diag = super::wait_season_markers_done(&app, &lib_id, media_id).await;
    assert_eq!(diag["paused"], true);
    assert_eq!(diag["job"]["status"], "succeeded");
    state.probe.shutdown_workers().await.unwrap();
}
