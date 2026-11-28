//! Round-2 review TDD tests: library-scoped marker diagnostics & ownership boundaries (Item 2),
//! cross-library scoped cancel isolation (Item 3 API), and cancelling queued single-episode
//! explicit media probes without touching running ffprobe or plain metadata probes (Item 4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::{ProbeJobUnitSpec, ScannedLedger};
use tower::ServiceExt;

use super::{ControllableFingerprint, StubTracks, common, test_state};

struct GatedTracksProbe {
    blocker_path: PathBuf,
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: Arc<AtomicBool>,
    probed: Arc<Mutex<Vec<PathBuf>>>,
}

impl api::probe_manager::TracksProbe for GatedTracksProbe {
    fn probe(
        &self,
        path: &Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.probed.lock().push(path.to_path_buf());
        if path == self.blocker_path {
            if let Some(tx) = self.entered.lock().take() {
                let _ = tx.send(());
            }
            let started = Instant::now();
            while !self.release.load(Ordering::Acquire)
                && !control.is_cancelled()
                && started.elapsed() < Duration::from_secs(10)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        StubTracks.probe(path, control)
    }
}

fn insert_media_row(store: &api::Store, kind: MediaKind, title: &str) -> MediaId {
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind,
            title: title.into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    media_id
}

fn insert_file_in_root(
    store: &api::Store,
    root: &Path,
    filename: &str,
    media_id: MediaId,
    unit: UnitKey,
    with_cached_meta: bool,
) -> (LedgerId, PathBuf) {
    let path = root.join(filename);
    std::fs::write(&path, format!("bytes-{filename}")).unwrap();
    let fid = LedgerId::new();
    let sv = api::fingerprint_job::current_source_version(&path);
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id: fid,
                media_id,
                path: path.to_string_lossy().into_owned(),
                units: vec![unit],
                resolution: Some("1080p".into()),
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
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
    if with_cached_meta {
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
    }
    (fid, path)
}

fn seed_two_libraries_shared_tv(
    store: &api::Store,
    root: &Path,
    with_cached_meta: bool,
) -> (String, String, MediaId, LedgerId, LedgerId) {
    let root_a = root.join("tv-a");
    let root_b = root.join("tv-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let lib_a = store
        .create_library(
            MediaKind::Tv,
            "Visible TV A",
            &[root_a.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib_b = store
        .create_library(
            MediaKind::Tv,
            "Private TV B",
            &[root_b.to_str().unwrap()],
            "members",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib_a.id, true, true)
        .unwrap();
    store
        .set_library_intro_settings(&lib_b.id, true, true)
        .unwrap();
    let shared = insert_media_row(store, MediaKind::Tv, "Shared Drama");
    let (fid_a, _) = insert_file_in_root(
        store,
        &root_a,
        "Shared.S01E01.A.mkv",
        shared,
        UnitKey::new(1, 1),
        with_cached_meta,
    );
    let (fid_b, _) = insert_file_in_root(
        store,
        &root_b,
        "Shared.S01E01.B.mkv",
        shared,
        UnitKey::new(1, 1),
        with_cached_meta,
    );
    (lib_a.id, lib_b.id, shared, fid_a, fid_b)
}

fn create_tv_marker_job(
    store: &api::Store,
    job_id: &str,
    kind: &str,
    media_id: MediaId,
    scope: &str,
    file_ids: &[LedgerId],
) {
    let id_strings: Vec<String> = file_ids.iter().map(ToString::to_string).collect();
    let specs: Vec<ProbeJobUnitSpec<'_>> = id_strings
        .iter()
        .map(|lid| ProbeJobUnitSpec {
            ledger_id: lid,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    assert!(
        store
            .create_probe_job(job_id, kind, &media_id.to_string(), Some(1), scope, &specs)
            .unwrap()
    );
}

async fn get_markers_json(app: &axum::Router, url: &str, token: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(common::request("GET", url, Some(token), Value::Null))
        .await
        .unwrap();
    let status = resp.status();
    (status, common::json_body(resp).await)
}

async fn post_json(app: &axum::Router, url: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(common::request(
            "POST",
            url,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, common::json_data(resp).await)
}

async fn wait_job_terminal(store: &api::Store, job_id: &str) -> store::ProbeJob {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let job = store.get_probe_job(job_id).unwrap().unwrap();
        if !job.is_active() {
            return job;
        }
        assert!(Instant::now() < deadline, "job {job_id} timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_scoped_marker_diagnostics_isolates_member_and_admin_from_other_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_a, _lib_b, shared, fid_a, fid_b) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);
    let (_uid, member_token) = common::create_member(&app, "marker-member-a").await;
    let url_a = format!("/api/v1/libraries/{lib_a}/items/{shared}/markers?season=1");

    // 1. Only private Library B has a job -> A must see job=null, active=false.
    create_tv_marker_job(
        &state.store(),
        "job-b-only",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_b}"),
        &[fid_b],
    );
    let (st, body_no_a) = get_markers_json(&app, &url_a, &member_token).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body_no_a["data"]["job"].is_null());
    assert_eq!(body_no_a["data"]["active"], false);

    // 2. A gets job_a, then B gets newer job_b2 -> A sees job_a, never job_b2.
    create_tv_marker_job(
        &state.store(),
        "job-a-first",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_a}"),
        &[fid_a],
    );
    let (fid_b2, _) = insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-b"),
        "Shared.S01E02.B.mkv",
        shared,
        UnitKey::new(1, 2),
        true,
    );
    create_tv_marker_job(
        &state.store(),
        "job-b-newer",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_b2}"),
        &[fid_b2],
    );
    let (st, body_a) = get_markers_json(&app, &url_a, &member_token).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body_a["data"]["job"]["id"], "job-a-first");

    // 3. Mixed-library job containing units from both A and B must NOT leak to A.
    let (fid_a3, _) = insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-a"),
        "Shared.S01E03.A.mkv",
        shared,
        UnitKey::new(1, 3),
        true,
    );
    let (fid_b3, _) = insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-b"),
        "Shared.S01E03.B.mkv",
        shared,
        UnitKey::new(1, 3),
        true,
    );
    create_tv_marker_job(
        &state.store(),
        "job-mixed-ab",
        "marker_refresh",
        shared,
        &format!("mixed:{shared}:1"),
        &[fid_a3, fid_b3],
    );
    let (_, body_after_mixed) = get_markers_json(&app, &url_a, &member_token).await;
    assert_eq!(body_after_mixed["data"]["job"]["id"], "job-a-first");

    // 4. Admin detect receipt on Library A must also return A's job, not B's or mixed job.
    let (st_det, receipt) = post_json(
        &app,
        &format!("/api/v1/libraries/{lib_a}/items/{shared}/markers/detect"),
        json!({ "season": 1, "episode": 1, "file_id": fid_a.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_det, StatusCode::OK);
    assert_eq!(receipt["job"]["id"], "job-a-first");
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marker_diagnostics_enforces_library_item_ownership_for_tv_and_movie() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_a, _lib_b, shared, _fid_a, _fid_b) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);
    let root_priv_movie = dir.path().join("priv-movie");
    let root_vis_movie = dir.path().join("vis-movie");
    std::fs::create_dir_all(&root_priv_movie).unwrap();
    std::fs::create_dir_all(&root_vis_movie).unwrap();
    let vis_movie_lib = state
        .store()
        .create_library(
            MediaKind::Movie,
            "Visible Movies",
            &[root_vis_movie.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let _priv_movie_lib = state
        .store()
        .create_library(
            MediaKind::Movie,
            "Private Movies",
            &[root_priv_movie.to_str().unwrap()],
            "members",
            true,
            &[],
        )
        .unwrap();
    let private_tv = insert_media_row(&state.store(), MediaKind::Tv, "Private Secret TV");
    insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-b"),
        "Private.S01E01.mkv",
        private_tv,
        UnitKey::new(1, 1),
        true,
    );
    let private_movie = insert_media_row(&state.store(), MediaKind::Movie, "Private Secret Movie");
    insert_file_in_root(
        &state.store(),
        &root_priv_movie,
        "Private.Movie.2024.mkv",
        private_movie,
        UnitKey::WHOLE,
        true,
    );
    let (_uid, member_token) = common::create_member(&app, "probe-boundary-member").await;

    let (st_tv, body_tv) = get_markers_json(
        &app,
        &format!("/api/v1/libraries/{lib_a}/items/{private_tv}/markers?season=1"),
        &member_token,
    )
    .await;
    assert_eq!(st_tv, StatusCode::NOT_FOUND);
    assert_eq!(body_tv["error"]["code"], "library.item_missing");

    let (st_mv, body_mv) = get_markers_json(
        &app,
        &format!(
            "/api/v1/libraries/{}/items/{private_movie}/markers",
            vis_movie_lib.id
        ),
        &member_token,
    )
    .await;
    assert_eq!(st_mv, StatusCode::NOT_FOUND);
    assert_eq!(body_mv["error"]["code"], "library.item_missing");

    let (st_s2, body_s2) = get_markers_json(
        &app,
        &format!("/api/v1/libraries/{lib_a}/items/{shared}/markers?season=2"),
        &member_token,
    )
    .await;
    assert_eq!(st_s2, StatusCode::OK);
    assert_eq!(body_s2["data"]["season"], 2);
    assert_eq!(body_s2["data"]["episodes"].as_array().unwrap().len(), 0);
    assert!(body_s2["data"]["job"].is_null());
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_scoped_cancel_terminates_only_target_library_while_sibling_completes() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_a, lib_b, shared, fid_a, fid_b) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);

    let (st_a, det_a) = post_json(
        &app,
        &format!("/api/v1/libraries/{lib_a}/items/{shared}/markers/detect"),
        json!({ "season": 1, "episode": 1, "file_id": fid_a.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_a, StatusCode::OK);
    let job_a = det_a["job"]["id"].as_str().unwrap().to_string();

    let (st_b, det_b) = post_json(
        &app,
        &format!("/api/v1/libraries/{lib_b}/items/{shared}/markers/detect"),
        json!({ "season": 1, "episode": 1, "file_id": fid_b.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_b, StatusCode::OK);
    let job_b = det_b["job"]["id"].as_str().unwrap().to_string();
    assert_ne!(job_a, job_b);

    let (st_c, cancel) = post_json(
        &app,
        "/api/v1/probe/fingerprint/cancel",
        json!({
            "library_id": lib_a,
            "media_id": shared.to_string(),
            "season": 1,
            "include_running": true
        }),
    )
    .await;
    assert_eq!(st_c, StatusCode::OK);
    assert_eq!(cancel["cancelled_units"], 1);
    assert_eq!(
        state.store().get_probe_job(&job_a).unwrap().unwrap().status,
        "cancelled"
    );
    assert_eq!(
        state.store().get_probe_job(&job_b).unwrap().unwrap().status,
        "queued"
    );

    state.probe.try_start_workers();
    assert_eq!(
        wait_job_terminal(&state.store(), &job_b).await.status,
        "succeeded"
    );
    state.probe.shutdown_workers().await.unwrap();
}

struct GatedTracksEnv {
    state: api::ApiState,
    entered_rx: std::sync::mpsc::Receiver<()>,
    release: Arc<AtomicBool>,
    probed: Arc<Mutex<Vec<PathBuf>>>,
}

fn state_with_gated_tracks(root: &Path, blocker_path: PathBuf) -> GatedTracksEnv {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let probed = Arc::new(Mutex::new(Vec::new()));
    let tracks = Arc::new(GatedTracksProbe {
        blocker_path,
        entered: Mutex::new(Some(entered_tx)),
        release: release.clone(),
        probed: probed.clone(),
    });
    let fp = ControllableFingerprint::fast();
    let root_buf = root.to_path_buf();
    let base = std::thread::spawn(move || {
        common::state(
            &root_buf,
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(root_buf.join("dl"))),
        )
    })
    .join()
    .unwrap();
    GatedTracksEnv {
        state: base.with_probe_engines(fp.clone(), fp, tracks).unwrap(),
        entered_rx,
        release,
        probed,
    }
}

struct ThreeUncachedEpisodes {
    lib_id: String,
    media_id: MediaId,
    blocker_fid: LedgerId,
    explicit_fid: LedgerId,
    explicit_path: PathBuf,
    plain_fid: LedgerId,
    plain_path: PathBuf,
}

fn seed_three_uncached_episodes(store: &api::Store, tv_root: &Path) -> ThreeUncachedEpisodes {
    let lib = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media_id = insert_media_row(store, MediaKind::Tv, "Queue Test");
    let (blocker_fid, _) = insert_file_in_root(
        store,
        tv_root,
        "Blocker.S01E01.mkv",
        media_id,
        UnitKey::new(1, 1),
        false,
    );
    let (explicit_fid, explicit_path) = insert_file_in_root(
        store,
        tv_root,
        "Explicit.S01E02.mkv",
        media_id,
        UnitKey::new(1, 2),
        false,
    );
    let (plain_fid, plain_path) = insert_file_in_root(
        store,
        tv_root,
        "Plain.S01E03.mkv",
        media_id,
        UnitKey::new(1, 3),
        false,
    );
    ThreeUncachedEpisodes {
        lib_id: lib.id,
        media_id,
        blocker_fid,
        explicit_fid,
        explicit_path,
        plain_fid,
        plain_path,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_queued_single_episode_media_probe_preserves_plain_metadata_and_running_ffprobe() {
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    let env = state_with_gated_tracks(dir.path(), tv_root.join("Blocker.S01E01.mkv"));
    let state = env.state;
    let app = api::router(state.clone());
    let seeded = seed_three_uncached_episodes(&state.store(), &tv_root);

    state.probe.try_start_workers();
    let blocker_row = state
        .store()
        .file_coverage_by_id(seeded.blocker_fid)
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(
            &blocker_row,
            api::probe_manager::policy::ProbeRequestOrigin::Detail,
        )
        .unwrap();
    env.entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("blocker must enter metadata probe");

    assert!(
        state
            .store()
            .create_probe_job(
                "plain-meta-job",
                "media_probe",
                &seeded.media_id.to_string(),
                Some(1),
                &format!("ledger:{}", seeded.plain_fid),
                &[ProbeJobUnitSpec {
                    ledger_id: &seeded.plain_fid.to_string(),
                    kind: "tv",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: false,
                }],
            )
            .unwrap()
    );
    let detect_url = format!(
        "/api/v1/libraries/{}/items/{}/markers/detect",
        seeded.lib_id, seeded.media_id
    );
    let (st_det, _) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 2, "file_id": seeded.explicit_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_det, StatusCode::OK);

    for untouched in [seeded.blocker_fid, seeded.plain_fid] {
        let (st, data) = post_json(
            &app,
            "/api/v1/probe/fingerprint/cancel",
            json!({ "file_id": untouched.to_string(), "include_running": true }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(data["cancelled_units"], 0);
    }

    let (_, once) = post_json(
        &app,
        "/api/v1/probe/fingerprint/cancel",
        json!({ "file_id": seeded.explicit_fid.to_string(), "include_running": true }),
    )
    .await;
    assert_eq!(once["cancelled_units"], 1);
    let (_, repeat) = post_json(
        &app,
        "/api/v1/probe/fingerprint/cancel",
        json!({ "file_id": seeded.explicit_fid.to_string(), "include_running": true }),
    )
    .await;
    assert_eq!(repeat["cancelled_units"], 0);

    env.release.store(true, Ordering::Release);
    state.probe.wake_dispatch();
    assert_eq!(
        wait_job_terminal(&state.store(), "plain-meta-job")
            .await
            .status,
        "succeeded"
    );
    assert!(env.probed.lock().contains(&seeded.plain_path));
    assert!(!env.probed.lock().contains(&seeded.explicit_path));

    let (st_re, resubmit) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 2, "file_id": seeded.explicit_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_re, StatusCode::OK);
    let re_job = resubmit["job"]["id"].as_str().unwrap();
    assert_eq!(
        wait_job_terminal(&state.store(), re_job).await.status,
        "succeeded"
    );
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_with_include_running_false_preserves_active_fingerprint_capture() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    state.probe.try_start_workers();
    let app = api::router(state.clone());

    let (lib_id, media_id, ep_ids, _) =
        super::seed_tv_library_and_episodes(&state.store(), dir.path(), &[1], false);
    let cov = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&cov, api::probe_manager::policy::ProbeRequestOrigin::Detail)
        .unwrap();

    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("capture must start");

    let (st, receipt) = post_json(
        &app,
        "/api/v1/probe/fingerprint/cancel",
        json!({
            "library_id": lib_id,
            "media_id": media_id.to_string(),
            "season": 1,
            "file_id": ep_ids[0].to_string(),
            "include_running": false,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(receipt["cancelled_units"], 0);
    assert_eq!(receipt["aborted_running"], 0);

    fp.block_capture.store(false, Ordering::Release);
    super::wait_until_unit_inactive(
        &state.store(),
        ep_ids[0],
        "active capture must complete when include_running=false",
    )
    .await;
    let unit = state
        .store()
        .latest_probe_unit_for_ledger(&ep_ids[0].to_string())
        .unwrap()
        .unwrap();
    assert_eq!(unit.status, "succeeded");
    state.probe.shutdown_workers().await.unwrap();
}
