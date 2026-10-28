use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

#[path = "executor_library_admin/artwork_writes.rs"]
mod artwork_writes;
#[path = "executor_library_admin/images.rs"]
mod images;
#[path = "executor_library_admin/resolvers.rs"]
mod resolvers;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    token: String,
    file: domain::LibraryFile,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "admin-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let token = "admin-route-token".to_string();
        let handle = state.store();
        let store = handle.lock();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "admin-route".into(),
            enabled: true,
            role: UserRole::Admin,
        };
        store.insert_user_with_password(&user, "password").unwrap();
        store.set_user_token(user.id, &token).unwrap();
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: "Admin fixture".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
        std::fs::create_dir_all(&library.root_paths[0]).unwrap();
        let path = library.root_paths[0].join("fixture.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        drop(store);
        Self {
            _dir: dir,
            state,
            token,
            file,
        }
    }

    fn deletion_uri(&self, item: bool) -> String {
        if item {
            format!(
                "/api/v1/libraries/{}/items/{}",
                self.file.library_id, self.file.media_id
            )
        } else {
            format!("/api/v1/ledger/{}?delete_file=true", self.file.file_id)
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn request(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[test]
fn admin_read_routes_keep_executor_responsive_under_store_contention() {
    for uri in [
        "/api/v1/ledger",
        "/api/v1/unidentified",
        "/api/v1/library/pending-deletions",
    ] {
        let f = Fixture::new();
        let store = f.state.store();
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = store.lock();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let app = router(f.state.clone());
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request("GET", uri, &f.token)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        holder.join().unwrap();
        assert_eq!(response.unwrap().status(), StatusCode::OK, "{uri}");
        assert!(
            elapsed < Duration::from_millis(200),
            "{uri} parked timer for {elapsed:?}"
        );
    }
}

#[test]
fn physical_deletion_refuses_exhausted_filesystem_budget_before_facts_change() {
    for item in [false, true] {
        let f = Fixture::new();
        let _one = f.state.fs_work.try_reserve().unwrap();
        let _two = f.state.fs_work.try_reserve().unwrap();
        let response = runtime()
            .block_on(router(f.state.clone()).oneshot(request(
                "DELETE",
                &f.deletion_uri(item),
                &f.token,
            )))
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let handle = f.state.store();
        let store = handle.lock();
        assert!(store.file_coverage_by_id(f.file.file_id).unwrap().is_some());
        assert!(store.list_pending_file_deletions().unwrap().is_empty());
        assert_eq!(std::fs::read(&f.file.path).unwrap(), b"fixture");
    }
}

#[test]
fn physical_deletion_purges_orphan_media_and_finishes_durable_intent() {
    for item in [false, true] {
        let f = Fixture::new();
        let response = runtime()
            .block_on(router(f.state.clone()).oneshot(request(
                "DELETE",
                &f.deletion_uri(item),
                &f.token,
            )))
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let handle = f.state.store();
        let store = handle.lock();
        assert!(store.file_coverage_by_id(f.file.file_id).unwrap().is_none());
        assert!(store.list_pending_file_deletions().unwrap().is_empty());
        // Its only file is gone, so the orphan Media is purged too (2026-10-29 E2E).
        assert!(store.get_media(f.file.media_id).unwrap().is_none());
        assert!(!f.file.path.exists());
    }
}

#[test]
fn admin_cleanup_failure_reports_pending_conflict_and_retry_finishes_intent() {
    let f = Fixture::new();
    let db =
        rusqlite::Connection::open(f.state.store().lock().data_dir().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_cleanup BEFORE DELETE ON library_files
        BEGIN SELECT RAISE(ABORT,'cleanup unavailable'); END;",
    )
    .unwrap();
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request(
            "DELETE",
            &f.deletion_uri(false),
            &f.token,
        )))
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = runtime().block_on(async {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    });
    assert_eq!(body["error"]["code"], "file.delete_pending");
    assert!(!f.file.path.exists());
    assert!(
        f.state
            .store()
            .lock()
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.state
            .store()
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .len(),
        1
    );
    db.execute_batch("DROP TRIGGER fail_cleanup").unwrap();
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request(
            "DELETE",
            &f.deletion_uri(false),
            &f.token,
        )))
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        f.state
            .store()
            .lock()
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_none()
    );
    assert!(
        f.state
            .store()
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn physical_deletion_preserves_a_source_changed_without_a_new_file_row() {
    for item in [false, true] {
        let f = Fixture::new();
        std::thread::sleep(Duration::from_millis(2));
        std::fs::write(&f.file.path, b"changed").unwrap();
        let response = runtime()
            .block_on(router(f.state.clone()).oneshot(request(
                "DELETE",
                &f.deletion_uri(item),
                &f.token,
            )))
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&f.file.path).unwrap(), b"changed");
        assert!(
            f.state
                .store()
                .lock()
                .file_coverage_by_id(f.file.file_id)
                .unwrap()
                .is_some()
        );
        assert!(
            f.state
                .store()
                .lock()
                .list_pending_file_deletions()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn retransfer_is_budgeted_and_publishes_the_actual_restored_source_version() {
    let f = Fixture::new();
    let source = f._dir.path().join("source.mkv");
    std::fs::write(&source, b"restored source").unwrap();
    let db =
        rusqlite::Connection::open(f.state.store().lock().data_dir().join("library.db")).unwrap();
    db.execute("UPDATE library_files SET source_path=?2,presence_state='missing',missing_at=1 WHERE file_id=?1",
        rusqlite::params![f.file.file_id.to_string(),source.display().to_string()]).unwrap();
    std::fs::remove_file(&f.file.path).unwrap();
    let uri = format!("/api/v1/ledger/{}/retransfer", f.file.file_id);
    let one = f.state.fs_work.try_reserve().unwrap();
    let two = f.state.fs_work.try_reserve().unwrap();
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request("POST", &uri, &f.token)))
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!f.file.path.exists());
    drop((one, two));
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request("POST", &uri, &f.token)))
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"restored source");
    let observed = library::observe_source(&f.file.path).unwrap();
    let current = f
        .state
        .store()
        .lock()
        .file_coverage_by_id(f.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.file_id, f.file.file_id);
    assert_eq!(current.file.version.source_version, observed.source_version);
    assert_eq!(current.file.version.revision, f.file.version.revision + 1);
    assert_eq!(current.file.presence, domain::FilePresence::Present);
}

struct SlowMetadata(std::sync::mpsc::Sender<()>);

impl api::file_stat_read::FileStatRead for SlowMetadata {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        let _ = self.0.send(());
        std::thread::sleep(Duration::from_millis(300));
        std::fs::metadata(path)
    }
}

#[test]
fn ledger_filesystem_observation_releases_store_and_executor() {
    let mut f = Fixture::new();
    let (entered, started) = std::sync::mpsc::channel();
    f.state = f.state.with_file_stat_read(Arc::new(SlowMetadata(entered)));
    let store = f.state.store();
    let observer = std::thread::spawn(move || {
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let guard = store
            .try_lock_for(Duration::from_millis(100))
            .expect("metadata held Store");
        assert!(!guard.list_libraries().unwrap().is_empty());
    });
    let (response, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(
            router(f.state.clone()).oneshot(request("GET", "/api/v1/ledger", &f.token)),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    observer.join().unwrap();
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(200),
        "metadata blocked timer: {elapsed:?}"
    );
}
