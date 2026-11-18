//! Site/Downloader configuration and maintenance never park a Tokio worker on Store.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}
#[path = "executor_site_downloader_store/fixture.rs"]
mod fixture;
#[path = "executor_site_downloader_store/verification.rs"]
mod verification;
use api::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use fixture::{Fixture, route};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
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
        let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::App);
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
        let guard = store.as_ref();
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

#[test]
fn site_patch_rejects_snapshot_superseded_by_concurrent_disable() {
    let site = update_site_after_conflict("PATCH", json!({"name":"Renamed Site"}));
    assert!(!site.enabled);
}

#[test]
fn site_put_rejects_snapshot_superseded_by_concurrent_cookie_refresh() {
    let site = update_site_after_conflict(
        "PUT",
        json!({"name":"Renamed Site","url":"https://fixture.invalid/","profile_id":"mteam"}),
    );
    assert_eq!(site.cookie.as_deref(), Some("refreshed-cookie"));
}

fn update_site_after_conflict(method: &str, body: Value) -> domain::Site {
    let fixture = Fixture::new();
    let store = fixture.state.store();
    let site_id = fixture.site;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (update_tx, update_rx) = mpsc::channel();
    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            store
                .writers()
                .with_app(|app| {
                    ready_tx.send(()).unwrap();
                    update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let mut site = app.get_site(site_id)?.unwrap();
                    site.enabled = false;
                    site.cookie = Some("refreshed-cookie".into());
                    assert!(app.save_site(&site)?);
                    Ok(())
                })
                .unwrap();
        })
    };
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let pool = store.reader_pool(store::Db::App);
    let initially_idle = pool.available();
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    pool.set_query_progress_observer(Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
    })));

    let app = router(fixture.state.clone());
    let uri = fixture.site_uri("");
    let update = || request(method, &uri, body.clone());
    runtime().block_on(async {
        let release_writer = async {
            // Wait for auth, role, and the update's own read to finish.
            tokio::time::timeout(Duration::from_secs(3), async {
                while reads.load(Ordering::SeqCst) < 3 || pool.available() < initially_idle {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            pool.set_query_progress_observer(None);
            update_tx.send(()).unwrap();
            writer.join().unwrap();
        };
        let (response, ()) = tokio::join!(app.clone().oneshot(update()), release_writer);
        assert_eq!(response.unwrap().status(), StatusCode::CONFLICT);
        let site = store.get_site(site_id).unwrap().unwrap();
        assert_eq!(site.name, "Original Site");
        assert!(!site.enabled);
        assert_eq!(site.cookie.as_deref(), Some("refreshed-cookie"));

        let response = app.oneshot(update()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let site = store.get_site(site_id).unwrap().unwrap();
        assert_eq!(site.name, "Renamed Site");
        site
    })
}
