//! Item 8 regressions: manual marker PUT/DELETE must observe source files off the Tokio
//! async worker thread (via `state.fs_work`), honor `fs_work` backpressure, and reject
//! concurrent source changes with 409 without persisting stale markers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use serde_json::{Value, json};
use store::ScannedLedger;
use tower::ServiceExt;

use super::{ControllableFingerprint, common, test_state};

const STRM_URL: &[u8] = b"https://fixture.invalid/episode-1.mkv\n";

fn seed_strm_episode(store: &api::Store, root: &Path) -> (String, MediaId, LedgerId, PathBuf) {
    let tv_root = root.join("tv_strm");
    std::fs::create_dir_all(&tv_root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "STRM Manual",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "STRM Manual Test".into(),
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
    let path = tv_root.join("Show.S01E01.strm");
    std::fs::write(&path, STRM_URL).unwrap();
    let fid = LedgerId::new();
    let sv = api::fingerprint_job::current_source_version(&path);
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id: fid,
                media_id,
                path: path.to_string_lossy().into_owned(),
                units: vec![UnitKey::new(1, 1)],
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
    (lib.id, media_id, fid, path)
}

#[cfg(unix)]
fn replace_with_fifo(path: &Path) {
    std::fs::remove_file(path).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo must succeed");
    assert!(status.success(), "mkfifo exited non-zero");
}

#[cfg(unix)]
async fn assert_request_does_not_block_health(
    app: &axum::Router,
    fifo_path: &Path,
    request: axum::http::Request<axum::body::Body>,
    bytes_to_write: &'static [u8],
    expected_status: StatusCode,
) -> Value {
    use std::os::unix::fs::OpenOptionsExt;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let watchdog_fired = Arc::new(AtomicBool::new(false));
    let stop_waiter = Arc::new(AtomicBool::new(false));

    let writer_path = fifo_path.to_path_buf();
    let wf = watchdog_fired.clone();
    let sw = stop_waiter.clone();
    let writer_handle = std::thread::spawn(move || {
        let mut writer = loop {
            if sw.load(Ordering::Acquire) {
                return;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&writer_path)
            {
                Ok(file) => break file,
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("unexpected fifo writer open error: {e}"),
            }
        };
        let _ = entered_tx.send(());
        if release_rx.recv_timeout(Duration::from_secs(2)).is_err() {
            wf.store(true, Ordering::Release);
        }
        let _ = writer.write_all(bytes_to_write);
        drop(writer);
    });

    let app_req = app.clone();
    let req_task = tokio::spawn(async move { app_req.oneshot(request).await.unwrap() });

    entered_rx
        .await
        .expect("observe_source must open the .strm FIFO");
    assert!(
        !watchdog_fired.load(Ordering::Acquire),
        "single-threaded Tokio executor was blocked by observe_source until watchdog fired"
    );

    let health = app
        .clone()
        .oneshot(common::request("GET", "/api/v1/health", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(
        !watchdog_fired.load(Ordering::Acquire),
        "/api/v1/health must finish before observe_source is released"
    );
    assert!(
        !req_task.is_finished(),
        "manual marker request must still be waiting on FIFO while health returned"
    );

    let _ = release_tx.send(());
    let response = req_task.await.unwrap();
    stop_waiter.store(true, Ordering::Release);
    writer_handle.join().unwrap();

    assert_eq!(response.status(), expected_status);
    common::json_data(response).await
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn manual_put_and_delete_observe_source_do_not_block_single_threaded_tokio_executor() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_id, media_id, fid, strm_path) = seed_strm_episode(&state.store(), dir.path());
    replace_with_fifo(&strm_path);

    let put_req = common::request(
        "PUT",
        &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers"),
        Some("management-secret"),
        json!({
            "file_id": fid.to_string(),
            "season": 1,
            "episode": 1,
            "intro_start_ms": 10_000,
            "intro_end_ms": 40_000,
            "locked": true
        }),
    );
    let saved =
        assert_request_does_not_block_health(&app, &strm_path, put_req, STRM_URL, StatusCode::OK)
            .await;
    assert_eq!(saved["intro_start_ms"], 10_000);
    assert_eq!(saved["locked"], true);

    let del_req = common::request(
        "DELETE",
        &format!(
            "/api/v1/libraries/{lib_id}/items/{media_id}/markers?file_id={fid}&season=1&episode=1"
        ),
        Some("management-secret"),
        Value::Null,
    );
    let deleted =
        assert_request_does_not_block_health(&app, &strm_path, del_req, STRM_URL, StatusCode::OK)
            .await;
    assert_eq!(deleted["deleted"], true);
    assert!(
        state
            .store()
            .get_file_marker(fid, UnitKey::new(1, 1))
            .unwrap()
            .is_none()
    );
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_put_and_delete_honor_fs_work_busy_and_reject_concurrent_source_change() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    let app = api::router(state.clone());
    let (lib_id, media_id, fid, strm_path) = seed_strm_episode(&state.store(), dir.path());

    // Exhaust fs_work budget -> both PUT and DELETE must return 503 runtime.busy.
    let mut held = Vec::new();
    while let Ok(res) = state.fs_work.try_reserve() {
        held.push(res);
    }
    assert!(!held.is_empty(), "fs_work must have at least one permit");

    let put_busy = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers"),
            Some("management-secret"),
            json!({
                "file_id": fid.to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 5_000,
                "intro_end_ms": 35_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put_busy.status(), StatusCode::SERVICE_UNAVAILABLE);

    let del_busy = app
        .clone()
        .oneshot(common::request(
            "DELETE",
            &format!(
                "/api/v1/libraries/{lib_id}/items/{media_id}/markers?file_id={fid}&season=1&episode=1"
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(held);

    // Concurrent source file replacement -> 409 Conflict without persisting stale marker.
    std::fs::write(
        &strm_path,
        b"https://fixture.invalid/episode-1-replaced.mkv\n",
    )
    .unwrap();
    let conflict = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers"),
            Some("management-secret"),
            json!({
                "file_id": fid.to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 5_000,
                "intro_end_ms": 35_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert!(
        state
            .store()
            .get_file_marker(fid, UnitKey::new(1, 1))
            .unwrap()
            .is_none()
    );
    state.probe.shutdown_workers().await.unwrap();
}
