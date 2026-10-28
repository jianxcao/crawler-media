//! Manual mutation routes keep Store waits and external IO off the runtime.

#[path = "management/common.rs"]
mod common;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct SlowFetch {
    store: OnceLock<Arc<parking_lot::Mutex<Store>>>,
}
impl Fetcher for SlowFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        let handle = self.store.get().unwrap();
        assert!(
            handle.try_lock_for(Duration::from_millis(100)).is_some(),
            "Fetcher must run outside Store"
        );
        std::thread::sleep(Duration::from_millis(500));
        Ok(common::nexusphp().into())
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: ApiState,
    app: axum::Router,
    subscribe: String,
    media: String,
    library: String,
    path: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::build(None)
    }

    fn build(downloader: Option<Arc<dyn downloader::Downloader>>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(SlowFetch {
            store: OnceLock::new(),
        });
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher.clone(),
            downloader.unwrap_or_else(|| {
                Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")))
            }),
            tmp.path().join("library"),
        )
        .unwrap();
        fetcher.store.set(state.store()).ok().unwrap();
        let app = router(state.clone());
        let created = runtime().block_on(async {
            common::create_site(&app).await;
            common::create_subscribe(&app, "search").await
        });
        let subscribe = created["id"].as_str().unwrap().to_string();
        let shared = state.store();
        let store = shared.lock();
        let sub = store
            .get_subscribe(subscribe.parse().unwrap())
            .unwrap()
            .unwrap();
        let library = store
            .default_library(domain::MediaKind::Movie)
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();
        let path = library.root_paths[0].join("manual-claim.2024.mkv");
        std::fs::write(&path, b"manual bytes").unwrap();
        store
            .insert_unidentified(path.display().to_string(), domain::Confidence::Low)
            .unwrap();
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: sub.media_id,
                path: library.root_paths[0]
                    .join("probe.mkv")
                    .display()
                    .to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        drop(store);
        Self {
            _tmp: tmp,
            state,
            app,
            subscribe,
            media: sub.media_id.to_string(),
            library: library.id,
            path,
        }
    }

    fn request(&self, route: usize) -> axum::http::Request<axum::body::Body> {
        let (uri, body) = match route {
            0 => (
                "/api/v1/search/admit".into(),
                json!({"subscribe_id":self.subscribe,"enclosure":"https://pt.example/download.php?id=1&passkey=abc"}),
            ),
            1 => (
                "/api/v1/unidentified/claim".into(),
                json!({"path":self.path,"action":{"type":"bind_existing","media_id":self.media}}),
            ),
            2 => (
                "/api/v1/unidentified/claim".into(),
                json!({"path":self.path,"action":{"type":"identify_and_transfer","title":"Manual claim","kind":"movie","year":2024}}),
            ),
            _ => (
                format!(
                    "/api/v1/libraries/{}/items/{}/probe",
                    self.library, self.media
                ),
                Value::Null,
            ),
        };
        common::request("POST", &uri, Some("management-secret"), body)
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn contend(f: &Fixture, route: usize) {
    let handle = f.state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = handle.lock();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (response, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(f.app.clone().oneshot(f.request(route)), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            started.elapsed()
        })
    });
    holder.join().unwrap();
    let response = response.unwrap();
    let status = response.status();
    let body = runtime().block_on(common::json_body(response));
    assert_eq!(status, StatusCode::OK, "route={route}: {body}");
    assert!(
        elapsed < Duration::from_millis(200),
        "route={route} blocked runtime: {elapsed:?}"
    );
}

#[test]
fn manual_admit_does_not_block_runtime_on_store_or_slow_fetcher() {
    contend(&Fixture::new(), 0);
}
#[test]
fn bind_claim_does_not_block_runtime_on_store() {
    contend(&Fixture::new(), 1);
}
#[test]
fn transfer_claim_does_not_block_runtime_on_store() {
    contend(&Fixture::new(), 2);
}
#[test]
fn forced_probe_does_not_block_runtime_on_store() {
    contend(&Fixture::new(), 3);
}

#[test]
fn manual_routes_recheck_readonly_after_waiting_without_mutating() {
    for route in 0..4 {
        let f = Fixture::new();
        let handle = f.state.store();
        let state = f.state.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = handle.lock();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(250));
            let _ = state.with_instance_writable(false);
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let response = runtime()
            .block_on(f.app.clone().oneshot(f.request(route)))
            .unwrap();
        holder.join().unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "route={route}");
        let shared = f.state.store();
        let store = shared.lock();
        assert!(
            store
                .get_unidentified(f.path.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .load_pending(f.subscribe.parse().unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .file_coverage_by_path(f.path.to_str().unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn manual_routes_keep_busy_zero_effect_when_worker_capacity_is_full() {
    for route in 0..4 {
        let mut f = Fixture::new();
        let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
        let _reserved = budget.try_reserve().unwrap();
        f.state = f.state.with_http_work(budget);
        f.app = router(f.state.clone());
        let response = runtime()
            .block_on(f.app.clone().oneshot(f.request(route)))
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "route={route}"
        );
        let shared = f.state.store();
        let store = shared.lock();
        assert!(
            store
                .get_unidentified(f.path.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .load_pending(f.subscribe.parse().unwrap())
                .unwrap()
                .is_empty()
        );
    }
}

#[path = "executor_manual_operations/admitted_delivery.rs"]
mod admitted_delivery;
