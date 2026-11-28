//! TDD regressions for round-three items 1 & 2:
//! 1. Explicit single-episode marker detect runs through capture even when automatic
//!    `enable_fingerprint` is disabled on the Library (both hot and cold media-info cache).
//! 2. An unrelated plain `media_probe` on `ledger:{file_id}` returns `409 markers.conflict`
//!    instead of masquerading as `already_running`, while repeating the explicit request reuses it.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use domain::{MediaKind, UnitKey};
use serde_json::json;
use tower::ServiceExt;

use super::super::{ControllableFingerprint, common, test_state};
use super::{
    get_markers_json, insert_file_in_root, post_json, state_with_gated_tracks, wait_job_terminal,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_episode_runs_with_automatic_fingerprint_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    let app = api::router(state.clone());

    let tv_root = dir.path().join("tv-disabled");
    std::fs::create_dir_all(&tv_root).unwrap();
    let lib = state
        .store()
        .create_library(
            MediaKind::Tv,
            "Disabled Auto FP",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    state
        .store()
        .set_library_intro_settings(&lib.id, true, false)
        .unwrap();
    let media_id = domain::MediaId::new();
    state
        .store()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Explicit Single Ep".into(),
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

    // Hot-cache E01, cold-cache E02, and plain media-info control E03.
    let (hot_fid, hot_path) = insert_file_in_root(
        &state.store(),
        &tv_root,
        "Show.S01E01.mkv",
        media_id,
        UnitKey::new(1, 1),
        true,
    );
    let (cold_fid, cold_path) = insert_file_in_root(
        &state.store(),
        &tv_root,
        "Show.S01E02.mkv",
        media_id,
        UnitKey::new(1, 2),
        false,
    );
    let (plain_fid, plain_path) = insert_file_in_root(
        &state.store(),
        &tv_root,
        "Show.S01E03.mkv",
        media_id,
        UnitKey::new(1, 3),
        false,
    );

    state.probe.try_start_workers();
    let detect_url = format!(
        "/api/v1/libraries/{}/items/{media_id}/markers/detect",
        lib.id
    );
    let diag_url = format!(
        "/api/v1/libraries/{}/items/{media_id}/markers?season=1",
        lib.id
    );

    // 1. Hot-cache single episode must enqueue (not 409) and enter capture.
    let (st_hot, rec_hot) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 1, "file_id": hot_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(
        st_hot,
        StatusCode::OK,
        "hot-cache explicit detect failed: {rec_hot}"
    );
    assert_eq!(rec_hot["queued"], true);
    let hot_job = rec_hot["job"]["id"]
        .as_str()
        .expect("hot job id")
        .to_string();
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("hot-cache explicit detect must enter fingerprint capture");
    fp.block_capture.store(false, Ordering::Release);
    assert_eq!(
        wait_job_terminal(&state.store(), &hot_job).await.status,
        "succeeded"
    );
    assert!(fp.captured_paths.lock().contains(&hot_path));

    // 2. Cold-cache single episode must run metadata AND hand off to fingerprint capture.
    let entered_cold = fp.reset_entered();
    fp.block_capture.store(true, Ordering::Release);
    let (st_cold, rec_cold) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 2, "file_id": cold_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(
        st_cold,
        StatusCode::OK,
        "cold-cache explicit detect failed: {rec_cold}"
    );
    assert_eq!(rec_cold["queued"], true);
    tokio::task::spawn_blocking(move || entered_cold.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("cold-cache explicit detect must enter fingerprint capture after metadata");
    fp.block_capture.store(false, Ordering::Release);
    super::super::wait_until_unit_inactive(&state.store(), cold_fid, "cold-cache explicit capture")
        .await;
    assert!(fp.captured_paths.lock().contains(&cold_path));

    // Diagnostics must show terminal state for the season.
    let (st_diag, diag) = get_markers_json(&app, &diag_url, "management-secret").await;
    assert_eq!(st_diag, StatusCode::OK);
    assert_eq!(diag["data"]["active"], false);

    // 3. Control: ordinary media_probe in the same disabled Library must NOT run fingerprint capture.
    let plain_cov = state
        .store()
        .file_coverage_by_id(plain_fid)
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(
            &plain_cov,
            api::probe_manager::policy::ProbeRequestOrigin::Detail,
        )
        .unwrap();
    super::super::wait_until_unit_inactive(&state.store(), plain_fid, "plain media probe").await;
    assert!(
        !fp.captured_paths.lock().contains(&plain_path),
        "ordinary media probe must still obey enable_fingerprint=false"
    );

    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_media_probe_conflicts_with_episode_recapture() {
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("tv-conflict");
    std::fs::create_dir_all(&tv_root).unwrap();
    let ep1_path = tv_root.join("Conflict.S01E01.mkv");
    let env = state_with_gated_tracks(dir.path(), ep1_path.clone());
    let state = env.state.clone();
    let app = api::router(state.clone());

    let lib = state
        .store()
        .create_library(
            MediaKind::Tv,
            "Conflict TV",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    state
        .store()
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media_id = domain::MediaId::new();
    state
        .store()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Conflict Media".into(),
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
    let (ep1_fid, _) = insert_file_in_root(
        &state.store(),
        &tv_root,
        "Conflict.S01E01.mkv",
        media_id,
        UnitKey::new(1, 1),
        false,
    );

    state.probe.try_start_workers();

    // 1. Plain media-info refresh occupies `ledger:{ep1_fid}` and blocks inside GatedTracksProbe.
    let (st_mi, _) = post_json(
        &app,
        &format!("/api/v1/libraries/{}/media-info/refresh", lib.id),
        json!({ "mode": "missing", "request_id": uuid::Uuid::new_v4().to_string() }),
    )
    .await;
    assert_eq!(st_mi, StatusCode::OK);
    env.entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("plain media-info refresh must enter tracks probe");

    // 2. Explicit recapture on ep1 must get 409 markers.conflict, NOT 200 already_running.
    let detect_url = format!(
        "/api/v1/libraries/{}/items/{media_id}/markers/detect",
        lib.id
    );
    let conflict_resp = app
        .clone()
        .oneshot(common::request(
            "POST",
            &detect_url,
            Some("management-secret"),
            json!({ "season": 1, "episode": 1, "file_id": ep1_fid.to_string(), "mode": "recapture" }),
        ))
        .await
        .unwrap();
    let conflict_status = conflict_resp.status();
    let conflict_body = common::json_body(conflict_resp).await;
    assert_eq!(conflict_status, StatusCode::CONFLICT, "{conflict_body}");
    assert_eq!(
        conflict_body["error"]["code"], "markers.conflict",
        "{conflict_body}"
    );

    // 3. Control: repeating an explicit single-episode detect on a fresh ep2 returns already_running with exact job ID.
    let (ep2_fid, _) = insert_file_in_root(
        &state.store(),
        &tv_root,
        "Conflict.S01E02.mkv",
        media_id,
        UnitKey::new(1, 2),
        false,
    );
    state.probe.pause_background();
    let (st_first, first) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 2, "file_id": ep2_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_first, StatusCode::OK, "{first}");
    assert_eq!(first["queued"], true);
    let first_id = first["job"]["id"].as_str().expect("first explicit job id");

    let (st_dup, dup) = post_json(
        &app,
        &detect_url,
        json!({ "season": 1, "episode": 2, "file_id": ep2_fid.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_dup, StatusCode::OK, "{dup}");
    assert_eq!(dup["queued"], false);
    assert_eq!(dup["already_running"], true);
    assert_eq!(dup["job"]["id"], first_id);

    env.release.store(true, Ordering::Release);
    state.probe.shutdown_workers().await.unwrap();
}
