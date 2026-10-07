//! Tests proving admission reloads subscribe policy and existing pending downloads.

use std::sync::Arc;
use std::sync::mpsc;

use api::router;
use axum::http::StatusCode;
use domain::Torrent;
use downloader::{Downloader, DownloaderError, MemoryDownloader};
use indexer::{FetchRequest, Fetcher, IndexerError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{json_data, request, subscribe_payload};

struct PauseGateFetcher {
    started: Mutex<Option<mpsc::Sender<()>>>,
    resume: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Fetcher for PauseGateFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        if let Some(resume) = self.resume.lock().take() {
            if let Some(started) = self.started.lock().take() {
                let _ = started.send(());
            }
            let _ = resume.recv();
        }
        Ok(include_str!("../../../indexer/tests/fixtures/longwatch_s01.html").to_string())
    }
}

struct TrackingDownloader {
    inner: MemoryDownloader,
    added_torrents: Arc<Mutex<Vec<Torrent>>>,
}

impl Downloader for TrackingDownloader {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        self.added_torrents.lock().push(torrent.clone());
        self.inner.add(torrent)
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.inner.remove(torrent, delete_files)
    }

    fn completed_files(
        &self,
        torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        self.inner.completed_files(torrent)
    }
}

async fn setup_subscription_and_clean_jobs(
    app: &axum::Router,
    tmp_path: &std::path::Path,
) -> String {
    let mut body = subscribe_payload("search");
    body["media"] = json!({ "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" });
    body["coverage"] = json!({ "kind": "tv", "season": 1, "episode_from": 1 });

    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let sub = json_data(resp).await;
    let sub_id = sub["id"].as_str().unwrap().to_string();

    let jobs = rusqlite::Connection::open(tmp_path.join("data/jobs.db")).unwrap();
    jobs.execute("DELETE FROM jobs WHERE kind <> 'subscribe_search'", [])
        .unwrap();
    jobs.execute(
        "UPDATE job_defs SET enabled = 0 WHERE kind <> 'subscribe_search'",
        [],
    )
    .unwrap();
    sub_id
}

fn build_test_state(
    tmp_path: &std::path::Path,
    fetcher: Arc<PauseGateFetcher>,
    downloader: Arc<TrackingDownloader>,
) -> api::ApiState {
    api::ApiState::new(
        api::Store::open(tmp_path.join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp_path.join("library"),
    )
    .unwrap()
}

async fn trigger_tick_and_wait_fetcher(
    app: &axum::Router,
    started_rx: mpsc::Receiver<()>,
) -> tokio::task::JoinHandle<axum::response::Response> {
    let app_tick = app.clone();
    let tick_handle = tokio::spawn(async move {
        app_tick
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
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("search started")
    })
    .await
    .unwrap();

    tick_handle
}

#[tokio::test]
async fn season_policy_changed_during_search_is_reloaded_before_admission() {
    let tmp = tempfile::tempdir().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let fetcher = Arc::new(PauseGateFetcher {
        started: Mutex::new(Some(started_tx)),
        resume: Mutex::new(Some(resume_rx)),
    });

    let added_torrents = Arc::new(Mutex::new(Vec::new()));
    let downloader = Arc::new(TrackingDownloader {
        inner: MemoryDownloader::new(tmp.path().join("stage")),
        added_torrents: added_torrents.clone(),
    });

    let api_state = build_test_state(tmp.path(), fetcher, downloader);
    let app = router(api_state.clone());

    app.clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            super::common::site_payload(),
        ))
        .await
        .unwrap();

    let sub_id = setup_subscription_and_clean_jobs(&app, tmp.path()).await;
    app.clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/search"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    let tick_handle = trigger_tick_and_wait_fetcher(&app, started_rx).await;

    let patch_resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            json!({ "selected_seasons": [2] }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_resp.status(), StatusCode::OK);

    resume_tx.send(()).unwrap();

    let tick_res = tick_handle.await.unwrap();
    assert_eq!(tick_res.status(), StatusCode::OK);

    let added = added_torrents.lock().clone();
    assert!(
        added.iter().all(|t| !t.title.contains("S01")),
        "Admission must reload latest coverage policy and not admit S01 candidates: {:?}",
        added
    );
    let store = api_state.store();
    let pending = store.lock().load_pending(sub_id.parse().unwrap()).unwrap();
    assert!(
        pending.is_empty(),
        "Pending must be empty since S01 was rejected"
    );
}
