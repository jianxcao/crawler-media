use api::{ApiState, Store};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use domain::{LibraryFile, LibraryId, MediaKind, UnitKey};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::{
    provider::QueryTiming,
    query_metrics::{QueryMetrics, QueryMetricsSnapshot},
};
use std::sync::Arc;
use std::time::Instant;
use tower::ServiceExt;

#[allow(dead_code)] // Shared A9 fixture also exposes a protocol constructor unused here.
#[path = "../../../store/tests/query_work/support.rs"]
mod seed;

pub const TOKEN: &str = "management-secret";
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub state: ApiState,
    pub library: LibraryId,
    pub files: Vec<LibraryFile>,
}
impl Fixture {
    pub fn new(size: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let library = seed::seed(&store, MediaKind::Movie, size);
        let files = (0..100.min(size))
            .map(|i| {
                let id = uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("query-media-movie-{i}").as_bytes(),
                )
                .to_string()
                .parse()
                .unwrap();
                store
                    .library_file_page(library, id, None, 1)
                    .unwrap()
                    .files
                    .remove(0)
            })
            .collect();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        Self {
            dir,
            state,
            library,
            files,
        }
    }
    pub fn database(&self) -> std::path::PathBuf {
        self.dir.path().join("data/library.db")
    }
}

pub fn request(uri: &str, timing: QueryTiming, metrics: QueryMetrics) -> Request<Body> {
    let mut request = Request::get(uri)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("X-Emby-Device-Id", "benchmark-device")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(timing);
    request.extensions_mut().insert(metrics);
    request
}

#[allow(dead_code)] // Used by the ignored release harness; WAL proof shares the fixture.
pub async fn read(
    app: axum::Router,
    uri: String,
    diagnostic: bool,
) -> (f64, u16, QueryMetricsSnapshot) {
    let timing = QueryTiming::start();
    let metrics = QueryMetrics::new(timing, diagnostic);
    let response = app
        .oneshot(request(&uri, timing, metrics.clone()))
        .await
        .unwrap();
    let status = response.status().as_u16();
    let _body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        timing.started.elapsed().as_secs_f64() * 1000.,
        status,
        metrics.snapshot(),
    )
}

pub fn write_batch(state: &ApiState, files: &mut [LibraryFile], batch: usize) -> (f64, f64, f64) {
    for file in files.iter_mut() {
        file.size_bytes = Some(batch as u64 + 1);
    }
    let units = [UnitKey::WHOLE];
    let publications: Vec<_> = files
        .iter()
        .map(|file| store::library_write::FilePublication {
            file,
            units: &units,
            expected_version: Some(&file.version),
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    let shared = state.store();
    let start = Instant::now();
    let (wait, held) = shared
        .library_write(|store| {
            let wait = start.elapsed().as_secs_f64() * 1000.;
            let held = Instant::now();
            let scope = store::library_write::WriteScope {
                library_id: files[0].library_id,
                config_revision: store.library_config_revision(&files[0].library_id.to_string())?,
            };
            let result = store.publish_files(&scope, &publications, batch as i64)?;
            assert_eq!(result.updated, files.len());
            Ok((wait, held.elapsed().as_secs_f64() * 1000.))
        })
        .unwrap();
    (start.elapsed().as_secs_f64() * 1000., wait, held)
}

pub fn checkpoint(path: &std::path::Path, mode: &str) -> (i64, i64, i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    db.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .unwrap()
}
