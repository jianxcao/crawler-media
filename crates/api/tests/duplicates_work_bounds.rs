mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Media, MediaId, MediaKind, User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{provider::QueryTiming, query_metrics::QueryMetrics};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    library: String,
    uri: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "bootstrap".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let library = {
            let handle = state.store();
            let store = handle.as_ref();
            let user = User {
                id: UserId::new(),
                login: "duplicates".into(),
                enabled: true,
                role: UserRole::Admin,
            };
            store.insert_user(&user).unwrap();
            store.set_user_token(user.id, "duplicates-token").unwrap();
            store.default_library(MediaKind::Tv).unwrap().unwrap().id
        };
        let uri = format!("/api/v1/libraries/{library}/duplicates");
        Self {
            _dir: dir,
            state,
            library,
            uri,
        }
    }

    fn seed(&self, count: usize) {
        let handle = self.state.store();
        let store = handle.as_ref();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Work".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.library_id == self.library)
            .unwrap();
        let conn = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; BEGIN").unwrap();
        for n in 0..count {
            let file = domain::LedgerId::new().to_string();
            let path = format!("{}/{n}.mkv", root.path.display());
            conn.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,
                display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
                VALUES (?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'v1','probe','high')",
                rusqlite::params![file,self.library,root.id,media.id.to_string(),path]).unwrap();
            conn.execute(
                "INSERT INTO library_file_units(file_id,library_id,media_id,season,episode)
                VALUES (?1,?2,?3,1,?4)",
                rusqlite::params![file, self.library, media.id.to_string(), n as i32],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
    }
}

fn request(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("authorization", "Bearer duplicates-token")
        .body(Body::empty())
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn duplicates_do_not_park_the_executor_waiting_for_store() {
    let fixture = Fixture::new();
    let held = fixture.state.store();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(held.clone(), store::Db::Library);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (response, elapsed) = runtime().block_on(async {
        let start = Instant::now();
        tokio::join!(
            router(fixture.state.clone()).oneshot(request(&fixture.uri)),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                start.elapsed()
            }
        )
    });
    holder.join().unwrap();
    assert_eq!(response.unwrap().status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_millis(200),
        "timer parked for {elapsed:?}"
    );
}

struct CountingFiles {
    backing: std::path::PathBuf,
    calls: AtomicUsize,
}
impl api::file_stat_read::FileStatRead for CountingFiles {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, _: &Path) -> std::io::Result<std::fs::Metadata> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(&self.backing)
    }
}

fn measure(count: usize) -> u64 {
    let fixture = Fixture::new();
    fixture.seed(count);
    let backing = fixture._dir.path().join("observation");
    std::fs::write(&backing, b"fixture").unwrap();
    let files = Arc::new(CountingFiles {
        backing,
        calls: AtomicUsize::new(0),
    });
    let app = router(fixture.state.clone().with_file_stat_read(files.clone()));
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, true);
    let mut req = request(&fixture.uri);
    req.extensions_mut().insert(timing);
    req.extensions_mut().insert(metrics.clone());
    runtime().block_on(async {
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"], serde_json::json!([]));
    });
    assert_eq!(
        files.calls.load(Ordering::SeqCst),
        count,
        "one physical observation per file"
    );
    let snapshot = metrics.snapshot();
    let statements = snapshot
        .statements
        .expect("complete capture SQL must be measured");
    println!(
        "duplicates count={count} SQL={statements} VM={:?}",
        snapshot.vm_steps
    );
    statements
}

#[test]
fn duplicate_capture_reuses_units_instead_of_per_file_queries() {
    let small = measure(100);
    let large = measure(10_000);
    assert!(
        large <= small + 2,
        "same Media must not add per-file SQL: {small} -> {large}"
    );
}

struct SlowFiles {
    backing: std::path::PathBuf,
    entered: std::sync::mpsc::Sender<()>,
}
impl api::file_stat_read::FileStatRead for SlowFiles {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, _: &Path) -> std::io::Result<std::fs::Metadata> {
        let _ = self.entered.send(());
        std::thread::sleep(Duration::from_millis(400));
        std::fs::metadata(&self.backing)
    }
}

#[test]
fn duplicate_delete_observes_files_without_holding_store() {
    let fixture = Fixture::new();
    fixture.seed(2);
    let file_id = fixture.state.store().list_file_coverage().unwrap()[0]
        .file
        .file_id;
    let uri = format!("{}/{file_id}", fixture.uri);
    let backing = fixture._dir.path().join("slow-observation");
    std::fs::write(&backing, b"fixture").unwrap();
    let (entered, received) = std::sync::mpsc::channel();
    let app = router(
        fixture
            .state
            .clone()
            .with_file_stat_read(Arc::new(SlowFiles { backing, entered })),
    );
    let worker = std::thread::spawn(move || {
        let req = Request::delete(uri)
            .header("authorization", "Bearer duplicates-token")
            .body(Body::empty())
            .unwrap();
        runtime().block_on(app.oneshot(req)).unwrap().status()
    });
    let observed = received.recv_timeout(Duration::from_secs(2)).is_ok();
    let handle = fixture.state.store();
    let available = handle
        .writers()
        .with_writer_timeout(Duration::from_millis(100), store::Db::Library, |_| Ok(()))
        .is_ok();
    let status = worker.join().unwrap();
    assert!(
        observed,
        "delete must use the injected physical observation seam"
    );
    assert!(available, "slow physical observation must release Store");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "different units cannot prove redundant coverage"
    );
}
