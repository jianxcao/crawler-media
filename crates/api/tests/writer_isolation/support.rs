use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Coverage, FetchMode, Filter, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId,
    Torrent,
};
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

pub const TOKEN: &str = "writer-isolation-fixture";
pub const POSITIVE_SLO: Duration = Duration::from_secs(2);
pub const NEGATIVE_SLO: Duration = Duration::from_millis(200);
pub struct Offline;
struct StubPayloadFetcher;
impl api::download_service::fetcher::TorrentPayloadFetcher for StubPayloadFetcher {
    fn prepare(
        &self,
        source: &downloader::TorrentSource,
        purpose: downloader::SubmissionPurpose,
        policy: &net_guard::TargetPolicy,
    ) -> Result<downloader::PreparedTorrent, downloader::TorrentFetchError> {
        match source {
            downloader::TorrentSource::Http(http)
                if !http.url.starts_with("http://127.0.0.1")
                    && !http.url.starts_with("http://localhost") =>
            {
                let name = http.url.as_str();
                let bytes = format!(
                    "d8:announce20:https://tracker/test4:infod6:lengthi1024e4:name{}:{name}12:piece lengthi16384e6:pieces20:01234567890123456789ee",
                    name.len()
                );
                Ok(downloader::PreparedTorrent::TorrentFile(
                    downloader::parse_verified_torrent(bytes.as_bytes()).unwrap(),
                ))
            }
            _ => downloader::prepare_download_payload(
                &net_guard::NetGuardClient::new(),
                source,
                purpose,
                policy,
            ),
        }
    }
}
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(
            "offline writer isolation fixture".into(),
        ))
    }
}
impl api::catalog::Catalog for Offline {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct Gate {
    entered: std::sync::atomic::AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    pub fn block(&self) {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        let mut released = self.released.lock();
        self.changed
            .wait_while_for(&mut released, |done| !*done, Duration::from_secs(10));
        assert!(*released, "test must release Downloader IO");
    }
    pub async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.entered.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("Downloader IO must enter the gate");
    }
    pub fn release(&self) {
        *self.released.lock() = true;
        self.changed.notify_all();
    }
}
pub struct ReleaseOnDrop(pub Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct IdleDownloader;
impl Downloader for IdleDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

pub struct GatedDownloader {
    pub gate: Arc<Gate>,
    pub completed: PathBuf,
}
impl Downloader for GatedDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.gate.block();
        Ok(vec![self.completed.clone()])
    }
}

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub state: ApiState,
    router: std::sync::OnceLock<axum::Router>,
    pub media: MediaId,
    pub subscribe: SubscribeId,
    pub user: domain::UserId,
    pub root: PathBuf,
}
impl Fixture {
    /// Outside Tokio: disable unrelated automatic background drivers.
    pub fn new(downloader: Arc<dyn Downloader>) -> Self {
        Self::with_fetcher(downloader, Arc::new(Offline))
    }
    pub fn with_fetcher<F: Fetcher + 'static>(
        downloader: Arc<dyn Downloader>,
        fetcher: Arc<F>,
    ) -> Self {
        Self::with_fetcher_kind(downloader, fetcher, MediaKind::Movie)
    }
    pub fn with_fetcher_kind<F: Fetcher + 'static>(
        downloader: Arc<dyn Downloader>,
        fetcher: Arc<F>,
        kind: MediaKind,
    ) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        store::test_support::reconfigure_default_root(&store, kind, &root).unwrap();
        store.set_scrape_enabled(false).unwrap();
        store.set_transfer_mode("copy").unwrap();
        let media = Media {
            id: MediaId::new(),
            kind,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let filter = Filter::new(FilterId::new(), "Unrestricted", vec![]);
        store.insert_filter(&filter).unwrap();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            RegistrySnapshot::builtin().unwrap(),
            fetcher,
            downloader,
        )
        .unwrap()
        .with_torrent_payload_fetcher(Arc::new(StubPayloadFetcher))
        .with_catalog(Arc::new(Offline));
        let user = state.store().user_by_token(TOKEN).unwrap().unwrap().id;
        let subscribe = Subscribe {
            id: SubscribeId::new(),
            user_id: user,
            media_id: media.id,
            coverage: if kind == MediaKind::Tv {
                Coverage::Tv {
                    season: 1,
                    episode_from: 1,
                    episode_to: Some(2),
                }
            } else {
                Coverage::Movie
            },
            fetch_mode: FetchMode::Search,
            filter_id: filter.id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        };
        state.store().insert_subscribe(&subscribe).unwrap();
        // PATCH has a real Search JobDef to update; no fake Job-sync success.
        state
            .jobs()
            .acquire()
            .unwrap()
            .ensure_def(jobs::NewDef {
                kind: jobs::JobKind::SubscribeSearch,
                scope: jobs::JobScope::Subscribe(subscribe.id),
                name: "Writer isolation Subscribe".into(),
                enabled: true,
                schedule: Some(jobs::Schedule::Interval { secs: 1800 }),
                payload: serde_json::json!({"subscribe_id": subscribe.id}).to_string(),
                timeout_secs: None,
                concurrency_key: Some(format!("search:{}", subscribe.id)),
            })
            .unwrap();
        Self {
            temp,
            state,
            router: std::sync::OnceLock::new(),
            media: media.id,
            subscribe: subscribe.id,
            user,
            root,
        }
    }
    pub fn router(&self) -> axum::Router {
        self.router
            .get_or_init(|| api::router(self.state.clone()))
            .clone()
    }
    pub fn idle() -> Self {
        Self::new(Arc::new(IdleDownloader))
    }
    pub fn pending(&self) {
        let torrent = Torrent {
            id: None,
            site_id: domain::SiteId::new(),
            title: "The.Matrix.1999.1080p.WEB-DL".into(),
            enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            size_bytes: Some(1024),
            seeders: Some(1),
            free: false,
            hr: Some(false),
            imdb_id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        };
        self.state
            .store()
            .record_pending_submission(
                self.subscribe,
                100,
                &store::PendingDownload {
                    submitted_at: Some(1000),
                    torrent,
                    release_override: None,
                    downloader_id: None,
                },
            )
            .unwrap();
    }
    pub async fn request(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
