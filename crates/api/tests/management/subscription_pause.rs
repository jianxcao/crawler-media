use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::SubscribeId;
use downloader::{Downloader, DownloaderError, MemoryDownloader};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::str::FromStr;
use tower::ServiceExt;

use super::common::{create_site, json_body, json_data, request, subscribe_payload};

#[tokio::test]
async fn pausing_during_site_search_prevents_the_search_from_adding_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let fetcher = Arc::new(PauseGateFetcher {
        started: Mutex::new(Some(started_tx)),
        resume: Mutex::new(Some(resume_rx)),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher,
            downloader.clone(),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    create_site(&app).await;
    let created = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                subscribe_payload("search"),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    jobs.execute("DELETE FROM jobs WHERE kind <> 'subscribe_search'", [])
        .unwrap();
    jobs.execute(
        "UPDATE job_defs SET enabled = 0 WHERE kind <> 'subscribe_search'",
        [],
    )
    .unwrap();
    let queued = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/search"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::OK);

    let tick_app = app.clone();
    let tick = tokio::spawn(async move {
        tick_app
            .oneshot(request(
                "POST",
                "/api/v1/jobs/tick?now=1",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap()
    });
    tokio::task::spawn_blocking(move || {
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("site search started")
    })
    .await
    .unwrap();

    let paused = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "tracking_state": "paused" }),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);
    resume_tx.send(()).unwrap();
    assert_eq!(tick.await.unwrap().status(), StatusCode::OK);

    assert!(downloader.added().is_empty());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let id = SubscribeId::from_str(&id).unwrap();
    assert!(store.load_pending(id).unwrap().is_empty());
}

#[tokio::test]
async fn manual_search_rejects_a_paused_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(super::common::Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", super::common::nexusphp())]),
    });
    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher,
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    create_site(&app).await;
    let created = super::common::create_subscribe(&app, "search").await;
    let id = created["id"].as_str().unwrap().to_string();
    let paused = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "tracking_state": "paused" }),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);

    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/search"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "subscription.paused");
}

#[tokio::test]
async fn deleting_subscription_waits_for_search_submission_and_cleans_its_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = deletion_fixture(tmp.path()).await;
    let tick = spawn_search_tick(fixture.app.clone());
    tokio::task::spawn_blocking(move || {
        fixture
            .started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("torrent submission started")
    })
    .await
    .unwrap();

    let delete_app = fixture.app.clone();
    let delete_id = fixture.id.clone();
    let mut delete = tokio::spawn(async move {
        delete_app
            .oneshot(request(
                "DELETE",
                &format!("/api/v1/subscriptions/{delete_id}?delete_torrents=true"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap()
    });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut delete)
        .await
        .ok()
        .map(|result| result.unwrap());
    let delete_waited_for_submit = early.is_none();
    fixture.resume_tx.send(()).unwrap();
    let response = match early {
        Some(response) => response,
        None => delete.await.unwrap(),
    };
    let tick_response = tick.await.unwrap();

    assert!(
        delete_waited_for_submit,
        "delete must wait for the submit guard"
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tick_response.status(), StatusCode::OK);
    assert!(!fixture.downloader.inner.added().is_empty());
    assert_eq!(fixture.downloader.inner.removed().len(), 1);
    let store = Store::open(tmp.path().join("data")).unwrap();
    let id = SubscribeId::from_str(&fixture.id).unwrap();
    assert!(store.get_subscribe(id).unwrap().is_none());
    assert!(store.load_pending(id).unwrap().is_empty());
}

struct DeletionFixture {
    app: axum::Router,
    id: String,
    downloader: Arc<GatedDownloader>,
    started_rx: mpsc::Receiver<()>,
    resume_tx: mpsc::Sender<()>,
}

async fn deletion_fixture(root: &Path) -> DeletionFixture {
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let downloader = Arc::new(GatedDownloader {
        inner: MemoryDownloader::new(root.join("stage")),
        started: Mutex::new(Some(started_tx)),
        resume: Mutex::new(Some(resume_rx)),
    });
    let fetcher = Arc::new(super::common::Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", super::common::nexusphp())]),
    });
    let app = router(
        ApiState::new(
            Store::open(root.join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            fetcher,
            downloader.clone(),
            root.join("library"),
        )
        .unwrap(),
    );
    create_site(&app).await;
    let created = super::common::create_subscribe(&app, "search").await;
    let id = created["id"].as_str().unwrap().to_string();
    let jobs = Connection::open(root.join("data/jobs.db")).unwrap();
    jobs.execute("DELETE FROM jobs WHERE kind <> 'subscribe_search'", [])
        .unwrap();
    jobs.execute(
        "UPDATE job_defs SET enabled = 0 WHERE kind <> 'subscribe_search'",
        [],
    )
    .unwrap();
    app.clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/search"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    DeletionFixture {
        app,
        id,
        downloader,
        started_rx,
        resume_tx,
    }
}

fn spawn_search_tick(app: axum::Router) -> tokio::task::JoinHandle<axum::response::Response> {
    tokio::spawn(async move {
        app.oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap()
    })
}

struct GatedDownloader {
    inner: MemoryDownloader,
    started: Mutex<Option<mpsc::Sender<()>>>,
    resume: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Downloader for GatedDownloader {
    fn add(&self, torrent: &domain::Torrent) -> Result<(), DownloaderError> {
        if let Some(started) = self.started.lock().take() {
            started.send(()).unwrap();
            self.resume.lock().take().unwrap().recv().unwrap();
        }
        self.inner.add(torrent)
    }

    fn remove(&self, torrent: &domain::Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.remove(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &domain::Torrent) -> Result<Option<String>, DownloaderError> {
        self.inner.owned_identity(torrent)
    }

    fn remove_owned(
        &self,
        torrent: &domain::Torrent,
        delete_files: bool,
    ) -> Result<(), DownloaderError> {
        self.inner.remove_owned(torrent, delete_files)
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.delete_task(info_hash, delete_files)
    }

    fn completed_files(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        self.inner.completed_files(torrent)
    }
}

struct PauseGateFetcher {
    started: Mutex<Option<mpsc::Sender<()>>>,
    resume: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Fetcher for PauseGateFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        if let Some(resume) = self.resume.lock().take() {
            self.started.lock().take().unwrap().send(()).unwrap();
            resume.recv().unwrap();
        }
        Ok(super::common::nexusphp().to_string())
    }
}
