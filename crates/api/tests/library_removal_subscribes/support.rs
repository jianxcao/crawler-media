use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Coverage, FetchMode, Filter, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId,
    Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

struct Offline;
impl Fetcher for Offline {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline removal fixture".into()))
    }
}
impl api::catalog::Catalog for Offline {
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
}
struct CompletedFile(PathBuf);
impl Downloader for CompletedFile {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn remove(&self, _: &Torrent, _: bool) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![self.0.clone()])
    }
}

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub state: ApiState,
    pub removed: store::Library,
    pub default: store::Library,
    pub source: PathBuf,
    pub subscribe: SubscribeId,
    pub media: MediaId,
}
impl Fixture {
    /// Construct outside Tokio so unrelated probe/catalog background drivers stay off.
    pub fn new(kind: MediaKind) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        store::test_support::reconfigure_default_root(&store, kind, &temp.path().join("default"))
            .unwrap();
        let default = store.default_library(kind).unwrap().unwrap();
        std::fs::create_dir_all(&default.root_paths[0]).unwrap();
        let root = temp.path().join("removed");
        std::fs::create_dir_all(&root).unwrap();
        let removed = store
            .create_library(
                kind,
                "Bound Library",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store.set_default_library(&removed.id).unwrap();
        store.set_transfer_mode("copy").unwrap();
        store.set_scrape_enabled(false).unwrap();
        let title = match kind {
            MediaKind::Tv => "The Long Watch",
            _ => "The Matrix",
        };
        let media = Media {
            id: MediaId::new(),
            kind,
            title: title.into(),
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
        let filter = Filter::new(FilterId::new(), "No transfer restrictions", vec![]);
        store.insert_filter(&filter).unwrap();
        let subscribe = Subscribe {
            id: SubscribeId::new(),
            user_id: UserId::new(),
            media_id: media.id,
            coverage: if kind == MediaKind::Tv {
                Coverage::Tv {
                    season: 1,
                    episode_from: 1,
                    episode_to: Some(1),
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
            library_id: Some(removed.id.parse().unwrap()),
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        };
        store.insert_subscribe(&subscribe).unwrap();
        let name = if kind == MediaKind::Tv {
            "The.Long.Watch.S01E01.1080p.WEB-DL.mkv"
        } else {
            "The.Matrix.1999.1080p.WEB-DL.mkv"
        };
        let source = temp.path().join(name);
        std::fs::write(&source, b"finite completed download fixture").unwrap();
        let state = ApiState::new(
            store,
            "removal-subscribe-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Offline),
            Arc::new(CompletedFile(source.clone())),
            temp.path().join("library"),
        )
        .unwrap()
        .with_catalog(Arc::new(Offline));
        Self {
            temp,
            state,
            removed,
            default,
            source,
            subscribe: subscribe.id,
            media: media.id,
        }
    }
    pub fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }
    pub fn binding(&self) -> Option<domain::LibraryId> {
        self.state
            .store()
            .lock()
            .get_subscribe(self.subscribe)
            .unwrap()
            .unwrap()
            .library_id
    }
    pub fn raw_binding(&self) -> Option<String> {
        rusqlite::Connection::open(self.temp.path().join("data/subscribe.db"))
            .unwrap()
            .query_row(
                "SELECT library_id FROM subscribes WHERE id=?1",
                [self.subscribe.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }
    pub fn pending(&self, kind: MediaKind) {
        let torrent = Torrent {
            id: None,
            site_id: domain::SiteId::new(),
            title: if kind == MediaKind::Tv {
                "The.Long.Watch.S01E01.1080p.WEB-DL".into()
            } else {
                "The.Matrix.1999.1080p.WEB-DL".into()
            },
            enclosure: "magnet:?xt=urn:btih:finite-removal-transfer".into(),
            size_bytes: Some(1024),
            seeders: Some(1),
            free: false,
            hr: false,
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
            .lock()
            .record_pending_submission(
                self.subscribe,
                100,
                &api::store::PendingDownload {
                    submitted_at: Some(1000),
                    torrent,
                    release_override: None,
                    downloader_id: None,
                },
            )
            .unwrap();
    }
    pub fn succeeded_with_binding_gap(&self) -> store::LibraryRemoval {
        let handle = self.state.store();
        let store = handle.lock();
        let operation = store.begin_library_removal(&self.removed.id).unwrap();
        store
            .set_removal_state(&operation.operation_id, "cleaning", None)
            .unwrap();
        let receipt = store
            .clean_library_removal(&operation.operation_id)
            .unwrap();
        assert_eq!(receipt.state, "succeeded");
        assert!(store.get_library(&self.removed.id).unwrap().is_none());
        // Model the durable state between library commit and subscribe commit.
        rusqlite::Connection::open(self.temp.path().join("data/subscribe.db"))
            .unwrap()
            .execute(
                "UPDATE subscribes SET library_id=?2 WHERE id=?1",
                rusqlite::params![self.subscribe.to_string(), self.removed.id],
            )
            .unwrap();
        receipt
    }
}
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
pub fn request(method: &str, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer removal-subscribe-secret")
        .body(Body::empty())
        .unwrap()
}
pub async fn json(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
pub async fn removed(fixture: &Fixture, operation: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = fixture
                .app()
                .oneshot(request(
                    "GET",
                    &format!("/api/v1/library-removals/{operation}"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = json(response).await;
            assert_ne!(body["data"]["state"], "failed", "{body}");
            if body["data"]["state"] == "succeeded" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("finite Library removal must complete");
}
pub async fn detached(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.binding().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("successful removal recovery must detach Subscribe bindings");
    assert_eq!(fixture.raw_binding(), None);
}
