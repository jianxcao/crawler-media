//! Router-level proof that a paused Library page does not retain the writer Store mutex.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::QueryTiming;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn request(device: &'static str) -> Request<Body> {
    Request::get("/Items?Limit=0")
        .header("Authorization", "Bearer management-secret")
        .header("X-Emby-Device-Id", device)
        .body(Body::empty())
        .unwrap()
}

#[test]
fn paused_protocol_cold_stage_does_not_hold_writer_store_mutex() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = Arc::new(AtomicBool::new(true));
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_protocol_selection_barrier(Arc::new(move |_| {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state);
        let reader = tokio::spawn(app.clone().oneshot(request("reader")));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let writer = app.clone().oneshot(request("writer"));
        let result = tokio::time::timeout(Duration::from_millis(500), writer).await;
        release_tx.send(()).unwrap();
        assert_eq!(
            result
                .expect("binding must complete while reader is paused")
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(reader.await.unwrap().unwrap().status(), StatusCode::OK);
    });
}

#[test]
fn slow_facet_reader_does_not_hold_writer_store_mutex() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = Arc::new(AtomicBool::new(true));
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_read_progress_observer(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state);
        let path = format!("/api/v1/libraries/{library}/facets");
        let reader_request = Request::get(path)
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        let reader = tokio::spawn(app.clone().oneshot(reader_request));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let writer = tokio::time::timeout(
            Duration::from_millis(500),
            app.clone().oneshot(request("writer")),
        )
        .await;
        release_tx.send(()).unwrap();
        assert_eq!(
            writer
                .expect("binding must finish during facet SQL")
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(reader.await.unwrap().unwrap().status(), StatusCode::OK);
    });
}

#[test]
fn pool_closes_on_shutdown_and_releases_after_final_owner() {
    let dir = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let weak = state.library_read_pool_weak();
    let clone = state.clone();
    drop(state);
    assert!(weak.upgrade().is_some());
    clone.close_library_reads();
    assert!(matches!(
        weak.upgrade().unwrap().try_checkout(),
        Err(store::LibraryReadError::Closed)
    ));
    drop(clone);
    assert!(weak.upgrade().is_none());
}

#[test]
fn full_one_reader_pool_returns_503_and_recovers() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = Arc::new(AtomicBool::new(true));
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_read_pool_size(std::num::NonZeroUsize::new(1).unwrap())
    .unwrap()
    .with_library_read_progress_observer(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state);
        let request = || {
            Request::get(format!("/api/v1/libraries/{library}/facets"))
                .header("Authorization", "Bearer management-secret")
                .body(Body::empty())
                .unwrap()
        };
        let first_reader = tokio::spawn(app.clone().oneshot(request()));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let busy = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        release_tx.send(()).unwrap();
        assert_read_error_body(busy).await;
        assert_eq!(
            first_reader.await.unwrap().unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::OK
        );
    });
}

fn lock_wait_expires_at_stage(target: &'static str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (stage_release_tx, stage_release_rx) = mpsc::channel();
    let stage_release_rx = Mutex::new(stage_release_rx);
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_library_read_stage_barrier(Arc::new(move |stage| {
        if stage == target {
            entered_tx.send(()).unwrap();
            stage_release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state.clone());
        let started = Instant::now();
        let deadline = started + Duration::from_millis(250);
        let mut request = Request::get(format!("/api/v1/libraries/{library}/facets"))
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(QueryTiming { started, deadline });
        let reader = tokio::spawn(app.oneshot(request));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (writer_release_tx, writer_release_rx) = mpsc::channel();
        let store = state.store();
        let writer = std::thread::spawn(move || {
            let _guard = store.lock();
            locked_tx.send(()).unwrap();
            writer_release_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        });
        tokio::task::spawn_blocking(move || {
            locked_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        let progressed = Arc::new(AtomicBool::new(false));
        let task_progress = progressed.clone();
        tokio::spawn(async move {
            task_progress.store(true, Ordering::SeqCst);
        });
        stage_release_tx.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{target}"
        );
        assert!(progressed.load(Ordering::SeqCst));
        writer_release_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_read_error_body(response).await;
    });
}

async fn assert_read_error_body(response: axum::response::Response) {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "library.read");
}

#[test]
fn prepare_store_wait_uses_request_deadline() {
    lock_wait_expires_at_stage("prepare");
}

#[test]
fn validate_store_wait_uses_same_request_deadline() {
    lock_wait_expires_at_stage("validate");
}

#[test]
fn read_only_instance_serves_leased_reads_and_rejects_explicit_write() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_instance_writable(false);
    runtime.block_on(async {
        let app = router(state);
        let read = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/libraries/{library}/facets"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK);
        let write = app
            .oneshot(
                Request::post(format!("/api/v1/libraries/{library}/scan"))
                    .header("Authorization", "Bearer management-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(write.status(), StatusCode::CONFLICT);
    });
}

#[test]
fn dropped_caller_retains_worker_permit_and_lease_until_exit() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = Arc::new(AtomicBool::new(true));
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_http_work(budget.clone())
    .with_library_read_pool_size(std::num::NonZeroUsize::new(1).unwrap())
    .unwrap()
    .with_library_read_progress_observer(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }));
    runtime.block_on(async {
        let app = router(state);
        let request = || {
            Request::get(format!("/api/v1/libraries/{library}/facets"))
                .header("Authorization", "Bearer management-secret")
                .body(Body::empty())
                .unwrap()
        };
        let reader = tokio::spawn(app.clone().oneshot(request()));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        reader.abort();
        let _ = reader.await;
        assert_eq!(budget.available_permits(), 0);
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        release_tx.send(()).unwrap();
        let mut recovered = false;
        for _ in 0..20 {
            if budget.available_permits() == 1 {
                let response = app.clone().oneshot(request()).await.unwrap();
                if response.status() == StatusCode::OK {
                    recovered = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            recovered,
            "aborted worker must clean up before capacity returns"
        );
    });
}

#[test]
fn selected_page_cold_stage_timeout_never_publishes_stale_success() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id;
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "One".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("one.mkv");
    std::fs::write(&path, b"fixture").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let entered_hook = entered.clone();
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_protocol_selection_barrier(Arc::new(move |page| {
        assert_eq!(page.items.len(), 1);
        entered_hook.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(400));
    }));
    runtime.block_on(async {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        let mut request = Request::get(format!("/Items?ParentId={library}&Limit=1"))
            .header("Authorization", "Bearer management-secret")
            .header("X-Emby-Device-Id", "device-1")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(QueryTiming { started, deadline });
        let response = router(state).oneshot(request).await.unwrap();
        assert!(entered.load(Ordering::SeqCst));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    });
}

#[test]
fn shutdown_during_protocol_cold_stage_cannot_publish_page() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_protocol_selection_barrier(Arc::new(move |_| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }));
    runtime.block_on(async {
        let app = router(state.clone());
        let reader = tokio::spawn(app.oneshot(request("device-1")));
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        })
        .await
        .unwrap();
        state.close_library_reads();
        release_tx.send(()).unwrap();
        let response = reader.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    });
}
