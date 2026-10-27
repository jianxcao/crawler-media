//! C02: Transfer waits for Library idle outside the Subscribe guard and async
//! handlers never park a runtime thread on that guard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api::router;
use axum::http::StatusCode;
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state, subscribe_payload};

const CHILD_ENV: &str = "SUBSCRIBE_WAIT_LIVENESS_CHILD";

/// Run `name` in a child process with an external deadline, so a starved runtime
/// reports a failure instead of wedging the whole binary.
fn watched(name: &str) -> bool {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        return false;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, name)
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{name}: child failed: {status}");
            return true;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{name}: child did not finish within 60s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Setup {
    _tmp: tempfile::TempDir,
    state: api::management::ApiState,
    app: axum::Router,
    sub_id: domain::SubscribeId,
    leases: Vec<api::library_lifecycle::LibraryWorkLease>,
}

async fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(state.clone());
    let mut body = subscribe_payload("search");
    body["media"] = json!({ "kind": "movie", "title": "The Matrix", "year": 1999 });
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let sub_id: domain::SubscribeId = json_data(resp).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let pending = api::store::PendingDownload {
        submitted_at: Some(1000),
        torrent: domain::Torrent {
            id: None,
            site_id: domain::SiteId::new(),
            title: "The.Matrix.1999.1080p.mkv".into(),
            enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            size_bytes: Some(1024),
            seeders: Some(10),
            free: false,
            hr: false,
            imdb_id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        },
        release_override: None,
        downloader_id: None,
    };
    state
        .store()
        .lock()
        .record_pending_submission(sub_id, 100, &pending)
        .unwrap();
    // Hold every Library busy so Transfer has to wait for idle.
    let ids: Vec<String> = state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .map(|library| library.id)
        .collect();
    let leases = ids
        .iter()
        .map(|id| state.acquire_library_work(id).unwrap())
        .collect();
    Setup {
        _tmp: tmp,
        state,
        app,
        sub_id,
        leases,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn single_worker_patch_does_not_starve_transfer_drain() {
    const NAME: &str =
        "subscribe_wait_liveness::single_worker_patch_does_not_starve_transfer_drain";
    if watched(NAME) {
        return;
    }
    let Setup {
        state,
        app,
        sub_id,
        leases,
        _tmp,
    } = setup().await;
    let transfer_state = state.clone();
    let transfer =
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&transfer_state, sub_id));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let patch = tokio::spawn(app.clone().oneshot(request(
        "PATCH",
        &format!("/api/v1/subscriptions/{sub_id}"),
        Some("management-secret"),
        json!({ "pack_fallback_after_days": 0 }),
    )));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let health = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(request(
            "GET",
            "/api/v1/health",
            None,
            serde_json::Value::Null,
        )),
    )
    .await;
    let health_request_finished_before_deadline = health.is_ok();
    if !health_request_finished_before_deadline {
        eprintln!("STARVED: health request not served while Transfer waited");
        std::process::exit(3);
    }
    drop(leases);
    let patched = tokio::time::timeout(Duration::from_secs(20), patch).await;
    let Ok(Ok(Ok(patched))) = patched else {
        eprintln!("STARVED: patch did not finish after the Library went idle");
        std::process::exit(3);
    };
    assert_eq!(patched.status(), StatusCode::OK);
    transfer.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_while_waiting_is_revalidated() {
    const NAME: &str = "subscribe_wait_liveness::deleted_while_waiting_is_revalidated";
    if watched(NAME) {
        return;
    }
    let Setup {
        state,
        app,
        sub_id,
        leases,
        _tmp,
    } = setup().await;
    let transfer_state = state.clone();
    let transfer =
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&transfer_state, sub_id));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let deleted = tokio::time::timeout(
        Duration::from_secs(5),
        app.clone().oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            serde_json::Value::Null,
        )),
    )
    .await;
    let Ok(Ok(deleted)) = deleted else {
        eprintln!("BLOCKED: DELETE waited on a Transfer parked for Library idle");
        std::process::exit(3);
    };
    assert_eq!(deleted.status(), StatusCode::OK);
    drop(leases);
    transfer.await.unwrap().unwrap();
    let deleted_subscribe_after = state.store().lock().get_subscribe(sub_id).unwrap();
    assert!(deleted_subscribe_after.is_none());
}
