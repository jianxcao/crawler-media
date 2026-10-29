use api::catalog::Catalog;
use api::media_posters::PosterError;
use api::media_posters::http::ImageFetcher;
use api::media_posters::service::MediaPosterService;
use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

pub const IMAGE: &[u8] = &[0x89, b'P', b'N', b'G', 13, 10, 26, 10];

#[derive(Clone, Copy)]
pub enum Outcome {
    Image,
    Timeout,
    Upstream,
    Invalid,
}

pub struct FiniteSource {
    gate: (Mutex<bool>, Condvar),
    delay: Mutex<Duration>,
    outcome: Mutex<Outcome>,
    pub active: AtomicUsize,
    pub peak: AtomicUsize,
}
impl FiniteSource {
    pub fn new(gated: bool, delay: Duration, outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            gate: (Mutex::new(!gated), Condvar::new()),
            delay: Mutex::new(delay),
            outcome: Mutex::new(outcome),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
    pub fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
    pub fn succeed(&self) {
        *self.outcome.lock().unwrap() = Outcome::Image;
        *self.delay.lock().unwrap() = Duration::ZERO;
    }
    pub fn release_on_drop(self: &Arc<Self>) -> ReleaseSource {
        ReleaseSource(self.clone())
    }
}
pub struct ReleaseSource(Arc<FiniteSource>);
impl Drop for ReleaseSource {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ImageFetcher for FiniteSource {
    fn fetch(&self, _: &str) -> Result<(String, Vec<u8>), PosterError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        let (gate, changed) = &self.gate;
        let (released, timeout) = changed
            .wait_timeout_while(gate.lock().unwrap(), Duration::from_secs(2), |open| !*open)
            .unwrap();
        if timeout.timed_out() && !*released {
            return Err(PosterError::Timeout);
        }
        drop(released);
        let delay = *self.delay.lock().unwrap();
        std::thread::sleep(delay);
        match *self.outcome.lock().unwrap() {
            Outcome::Image => Ok(("image/png".into(), IMAGE.to_vec())),
            Outcome::Timeout => Err(PosterError::Timeout),
            Outcome::Upstream => Err(PosterError::Upstream("finite source unavailable".into())),
            Outcome::Invalid => Err(PosterError::InvalidImage),
        }
    }
}

struct FiniteCatalog;
impl Catalog for FiniteCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, id: &str) -> Result<Option<String>, String> {
        Ok(Some(format!("https://image.tmdb.org/t/p/w500/{id}.png")))
    }
}
struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub state: ApiState,
    pub service: Arc<MediaPosterService>,
    pub ids: Vec<MediaId>,
}
impl Fixture {
    /// Called before entering the runtime so unrelated manager loops never start.
    pub fn new(source: Arc<FiniteSource>, wait: Duration, count: usize) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let ids = (0..count)
            .map(|n| {
                let id = MediaId::new();
                store
                    .insert_media(&Media {
                        id,
                        kind: MediaKind::Movie,
                        title: format!("Finite poster {n}"),
                        year: None,
                        original_title: None,
                        aliases: vec![],
                        tmdb_id: Some(format!("finite-{n}")),
                        douban_id: None,
                        tvdb_id: None,
                        bangumi_id: None,
                        anilist_id: None,
                    })
                    .unwrap();
                id
            })
            .collect();
        let state = ApiState::new(
            store,
            "local-fixture".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(MemoryDownloader::new(temp.path().join("downloads"))),
            temp.path().join("library"),
        )
        .unwrap();
        let service = Arc::new(
            MediaPosterService::with_custom_fetcher(
                state.store(),
                Arc::new(FiniteCatalog),
                temp.path().join("posters"),
                source.clone(),
            )
            .unwrap()
            .with_wait_deadline(wait),
        );
        let state = state.with_media_posters(service.clone());
        Self {
            _temp: temp,
            state,
            service,
            ids,
        }
    }
    pub fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }
    pub fn cache_path(&self, index: usize) -> std::path::PathBuf {
        self._temp
            .path()
            .join("posters")
            .join(format!("{}.img", self.ids[index]))
    }
    pub async fn drained(&self) {
        until("poster workers and observers drain", || {
            let counts = self.service.resource_counts();
            counts.active_workers == 0
                && counts.registry_entries == 0
                && self
                    .ids
                    .iter()
                    .all(|id| self.service.inflight_waiters(*id) == 0)
        })
        .await;
    }
}
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
pub fn request(id: MediaId) -> Request<Body> {
    Request::get(format!("/api/v1/media/{id}/poster"))
        .body(Body::empty())
        .unwrap()
}
pub async fn image(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["cache-control"], "public, max-age=86400");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        IMAGE
    );
}
pub async fn get(app: axum::Router, id: MediaId) -> axum::response::Response {
    tokio::time::timeout(Duration::from_secs(1), app.oneshot(request(id)))
        .await
        .expect("finite image response must finish")
        .unwrap()
}
pub async fn until(label: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !ready() {
        assert!(Instant::now() < deadline, "{label}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
