//! Site/Downloader configuration and maintenance never park a Tokio worker on Store.
#[path = "executor_site_downloader_store/fixture.rs"]
mod fixture;
#[path = "executor_site_downloader_store/verification.rs"]
mod verification;
use api::router;
use axum::body::Body;
use axum::http::Request;
use fixture::{Fixture, route};
use serde_json::Value;
use std::time::{Duration, Instant};
use tower::ServiceExt;

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer site-downloader-secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn holder(f: &Fixture, readonly: bool) -> std::thread::JoinHandle<()> {
    let store = f.state.store();
    let state = f.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _guard = store.lock();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        if readonly {
            let _ = state.with_instance_writable(false);
        }
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    thread
}

#[test]
fn site_downloader_routes_keep_executor_responsive() {
    let mut blocked = Vec::new();
    for index in 0..23 {
        let f = Fixture::new();
        let (method, uri, body, expected) = route(&f, index);
        let app = router(f.state.clone());
        let held = holder(&f, false);
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, &uri, body)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        held.join().unwrap();
        assert_eq!(
            response.unwrap().status().as_u16(),
            expected,
            "{method} {uri}"
        );
        if elapsed >= Duration::from_millis(200) {
            blocked.push(format!("{method} {uri}: {elapsed:?}"));
        }
    }
    assert!(
        blocked.is_empty(),
        "Store blocked executor:\n{}",
        blocked.join("\n")
    );
}

#[test]
fn site_downloader_mutations_recheck_admission_after_lock_wait() {
    for index in 5..14 {
        let f = Fixture::new();
        let (method, uri, body, _) = route(&f, index);
        let app = router(f.state.clone());
        let held = holder(&f, true);
        let response = runtime()
            .block_on(app.oneshot(request(method, &uri, body)))
            .unwrap();
        held.join().unwrap();
        assert_eq!(response.status().as_u16(), 409, "{method} {uri}");
        let store = f.state.store();
        let guard = store.lock();
        assert_eq!(guard.list_sites().unwrap().len(), 1);
        assert_eq!(
            guard.get_site(f.site).unwrap().unwrap().name,
            "Original Site"
        );
        assert_eq!(guard.list_downloaders().unwrap().len(), 1);
        assert_eq!(
            guard.get_downloader(f.downloader).unwrap().unwrap().name,
            "Original Downloader"
        );
        assert!(
            guard
                .get_setting("download.target_prefs")
                .unwrap()
                .is_none()
        );
    }
}
