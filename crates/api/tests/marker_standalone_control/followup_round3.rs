//! Follow-up round 3 TDD regressions:
//! - Item 1 (P1): reject empty, whitespace-only, or explicit null cancel scope fields.
//! - Item 3 (P2): library-scoped season detect scope_key + exact submitted/reused job receipt.
//! - Item 4 (P2): season diagnostics prioritize active jobs over newer terminal jobs.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use domain::UnitKey;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::review_round2::{
    create_tv_marker_job, get_markers_json, insert_file_in_root, post_json,
    seed_two_libraries_shared_tv, wait_job_terminal,
};
use super::{ControllableFingerprint, common, test_state};

async fn post_cancel_raw(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/cancel",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, common::json_body(resp).await)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_rejects_empty_whitespace_and_null_scope_fields_without_aborting_work() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    let app = api::router(state.clone());
    let (lib_a, _lib_b, shared, fid_a, fid_b) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);

    create_tv_marker_job(
        &state.store(),
        "job-running-a",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_a}"),
        &[fid_a],
    );
    state.probe.try_start_workers();
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("job-running-a must enter capture");

    create_tv_marker_job(
        &state.store(),
        "job-queued-b",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_b}"),
        &[fid_b],
    );

    let invalid_payloads = [
        json!({ "file_id": " " }),
        json!({ "file_id": "" }),
        json!({ "file_id": Value::Null }),
        json!({ "library_id": lib_a, "file_id": "" }),
        json!({ "library_id": "   " }),
        json!({ "library_id": Value::Null }),
        json!({ "media_id": "" }),
        json!({ "media_id": "\t\n" }),
        json!({ "media_id": Value::Null }),
        json!({ "season": -1 }),
        json!({ "season": Value::Null }),
    ];
    for payload in invalid_payloads {
        let (status, body) = post_cancel_raw(&app, payload.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "payload={payload}");
        assert_eq!(body["error"]["code"], "probe.invalid", "payload={payload}");
        assert!(
            state
                .store()
                .get_probe_job("job-running-a")
                .unwrap()
                .unwrap()
                .is_active(),
            "running job must remain active after rejected payload={payload}"
        );
        assert!(
            state
                .store()
                .get_probe_job("job-queued-b")
                .unwrap()
                .unwrap()
                .is_active(),
            "queued job must remain active after rejected payload={payload}"
        );
    }

    // Release capture barrier and confirm both jobs finish cleanly.
    fp.block_capture.store(false, Ordering::Release);
    assert_eq!(
        wait_job_terminal(&state.store(), "job-running-a")
            .await
            .status,
        "succeeded"
    );
    assert_eq!(
        wait_job_terminal(&state.store(), "job-queued-b")
            .await
            .status,
        "succeeded"
    );

    // Valid scoped file_id cancel and explicit global cancel still work.
    fp.block_capture.store(true, Ordering::Release);
    create_tv_marker_job(
        &state.store(),
        "job-scoped-a",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_a}"),
        &[fid_a],
    );
    create_tv_marker_job(
        &state.store(),
        "job-scoped-b",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_b}"),
        &[fid_b],
    );
    let (st_f, body_f) = post_cancel_raw(
        &app,
        json!({ "file_id": fid_b.to_string(), "include_running": true }),
    )
    .await;
    assert_eq!(st_f, StatusCode::OK);
    assert_eq!(body_f["data"]["cancelled_units"], 1);
    assert!(
        state
            .store()
            .get_probe_job("job-scoped-a")
            .unwrap()
            .unwrap()
            .is_active()
    );

    let (st_g, body_g) = post_cancel_raw(&app, json!({ "include_running": true })).await;
    assert_eq!(st_g, StatusCode::OK);
    assert_eq!(body_g["data"]["cancelled_units"], 1);
    state.probe.shutdown_workers().await.unwrap();
}

async fn wait_job_stage_fingerprint(store: &api::Store, job_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if store
            .probe_job_units(job_id)
            .unwrap()
            .first()
            .is_some_and(|u| u.stage == store::ProbeDispatchStage::Fingerprint)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} never reached fingerprint stage"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn season_detect_is_library_scoped_and_returns_exact_submitted_or_reused_job() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    let app = api::router(state.clone());
    let (lib_a, lib_b, shared, fid_a, fid_b) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);
    state.probe.try_start_workers();

    let url_a = format!("/api/v1/libraries/{lib_a}/items/{shared}/markers/detect");
    let url_b = format!("/api/v1/libraries/{lib_b}/items/{shared}/markers/detect");

    let (st_a, receipt_a) =
        post_json(&app, &url_a, json!({ "season": 1, "mode": "recapture" })).await;
    assert_eq!(st_a, StatusCode::OK);
    assert_eq!(receipt_a["queued"], true);
    assert_eq!(receipt_a["already_running"], false);
    let job_a = receipt_a["job"]["id"]
        .as_str()
        .expect("job_a id")
        .to_string();

    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("library A refresh must start");

    // Library B must independently enqueue its own season refresh while A is still active.
    let (st_b, receipt_b) =
        post_json(&app, &url_b, json!({ "season": 1, "mode": "recapture" })).await;
    assert_eq!(st_b, StatusCode::OK);
    assert_eq!(receipt_b["queued"], true);
    assert_eq!(receipt_b["already_running"], false);
    let job_b = receipt_b["job"]["id"]
        .as_str()
        .expect("job_b id")
        .to_string();
    assert_ne!(job_a, job_b);

    let units_a = state.store().probe_job_units(&job_a).unwrap();
    let units_b = state.store().probe_job_units(&job_b).unwrap();
    assert_eq!(units_a.len(), 1);
    assert_eq!(units_a[0].ledger_id, fid_a.to_string());
    assert_eq!(units_b.len(), 1);
    assert_eq!(units_b[0].ledger_id, fid_b.to_string());

    // Repeating season detect on A reuses A's exact active job ID.
    let (st_rep, repeat_a) =
        post_json(&app, &url_a, json!({ "season": 1, "mode": "recapture" })).await;
    assert_eq!(st_rep, StatusCode::OK);
    assert_eq!(repeat_a["queued"], false);
    assert_eq!(repeat_a["already_running"], true);
    assert_eq!(repeat_a["job"]["id"], job_a);

    // Wait until library B's unit reaches the fingerprint queue (behind A), then cancel B only.
    wait_job_stage_fingerprint(&state.store(), &job_b).await;
    let (st_cb, _) = post_cancel_raw(
        &app,
        json!({ "library_id": lib_b, "media_id": shared.to_string(), "season": 1, "include_running": true }),
    )
    .await;
    assert_eq!(st_cb, StatusCode::OK);
    assert_eq!(
        wait_job_terminal(&state.store(), &job_b).await.status,
        "cancelled"
    );

    fp.block_capture.store(false, Ordering::Release);
    assert_eq!(
        wait_job_terminal(&state.store(), &job_a).await.status,
        "succeeded"
    );

    // Single-episode submit & repeat receipt must return THAT file's job, not a newer sibling job.
    let (fid_a2, _) = insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-a"),
        "Shared.S01E02.A.mkv",
        shared,
        UnitKey::new(1, 2),
        true,
    );
    fp.block_capture.store(true, Ordering::Release);
    let (st_e1, rec_e1) = post_json(
        &app,
        &url_a,
        json!({ "season": 1, "episode": 1, "file_id": fid_a.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_e1, StatusCode::OK);
    let job_e1 = rec_e1["job"]["id"].as_str().unwrap().to_string();

    let (st_e2, rec_e2) = post_json(
        &app,
        &url_a,
        json!({ "season": 1, "episode": 2, "file_id": fid_a2.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_e2, StatusCode::OK);
    let job_e2 = rec_e2["job"]["id"].as_str().unwrap().to_string();
    assert_ne!(job_e1, job_e2);

    let (st_e1_rep, rec_e1_rep) = post_json(
        &app,
        &url_a,
        json!({ "season": 1, "episode": 1, "file_id": fid_a.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_e1_rep, StatusCode::OK);
    assert_eq!(rec_e1_rep["already_running"], true);
    assert_eq!(rec_e1_rep["job"]["id"], job_e1);

    // Season-wide detect while a file is occupied by a single-episode job returns 409 conflict.
    let resp_conflict = app
        .clone()
        .oneshot(common::request(
            "POST",
            &url_a,
            Some("management-secret"),
            json!({ "season": 1, "mode": "recapture" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp_conflict.status(), StatusCode::CONFLICT);

    fp.block_capture.store(false, Ordering::Release);
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn season_markers_prioritizes_active_job_over_newer_cancelled_job_and_isolates_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp.clone());
    let app = api::router(state.clone());
    let (lib_a, _lib_b, shared, fid_a1, fid_b1) =
        seed_two_libraries_shared_tv(&state.store(), dir.path(), true);
    let (fid_a2, _) = insert_file_in_root(
        &state.store(),
        &dir.path().join("tv-a"),
        "Shared.S01E02.A.mkv",
        shared,
        UnitKey::new(1, 2),
        true,
    );
    state.probe.try_start_workers();

    let url_a_detect = format!("/api/v1/libraries/{lib_a}/items/{shared}/markers/detect");
    let url_a_diag = format!("/api/v1/libraries/{lib_a}/items/{shared}/markers?season=1");

    let (st_e1, rec_e1) = post_json(
        &app,
        &url_a_detect,
        json!({ "season": 1, "episode": 1, "file_id": fid_a1.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_e1, StatusCode::OK);
    let job_e1 = rec_e1["job"]["id"].as_str().unwrap().to_string();
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("E1 must enter running capture");

    // Submit newer E2 job in same library, then cancel E2 while E1 is still running.
    let (st_e2, rec_e2) = post_json(
        &app,
        &url_a_detect,
        json!({ "season": 1, "episode": 2, "file_id": fid_a2.to_string(), "mode": "recapture" }),
    )
    .await;
    assert_eq!(st_e2, StatusCode::OK);
    let job_e2 = rec_e2["job"]["id"].as_str().unwrap().to_string();
    let (st_c2, _) = post_cancel_raw(
        &app,
        json!({ "library_id": lib_a, "file_id": fid_a2.to_string(), "include_running": true }),
    )
    .await;
    assert_eq!(st_c2, StatusCode::OK);
    assert_eq!(
        wait_job_terminal(&state.store(), &job_e2).await.status,
        "cancelled"
    );

    // Diagnostics must still report active=true with running job E1, NOT newer cancelled job E2.
    let (st_d1, diag_active) = get_markers_json(&app, &url_a_diag, "management-secret").await;
    assert_eq!(st_d1, StatusCode::OK);
    assert_eq!(diag_active["data"]["active"], true);
    assert_eq!(diag_active["data"]["job"]["id"], job_e1);

    // Newer active job in Library B must NOT affect Library A's diagnostics.
    create_tv_marker_job(
        &state.store(),
        "job-b-active-newer",
        "fingerprint_probe",
        shared,
        &format!("ledger:{fid_b1}"),
        &[fid_b1],
    );
    let (_, diag_isolated) = get_markers_json(&app, &url_a_diag, "management-secret").await;
    assert_eq!(diag_isolated["data"]["job"]["id"], job_e1);

    // Once E1 finishes, A returns active=false and the most recently created eligible job in A.
    fp.block_capture.store(false, Ordering::Release);
    assert_eq!(
        wait_job_terminal(&state.store(), &job_e1).await.status,
        "succeeded"
    );
    let (_, diag_done) = get_markers_json(&app, &url_a_diag, "management-secret").await;
    assert_eq!(diag_done["data"]["active"], false);
    assert_eq!(diag_done["data"]["job"]["id"], job_e2);
    state.probe.shutdown_workers().await.unwrap();
}
