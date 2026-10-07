//! A01: legacy POST /subscriptions/{id}/run must honor owner isolation and
//! Subscribe lifecycle. The authenticated /search Job path already does this;
//! /run still executes Downloader add + Transfer from a stale snapshot.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError, MemoryDownloader};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::str::FromStr;
use tower::ServiceExt;

use super::common::{
    Fixtures, create_member, create_site, json_body, json_error, nexusphp, request, state,
    subscribe_payload,
};

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
        Ok(nexusphp().into())
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

fn movie_subscribe() -> Value {
    let mut body = subscribe_payload("search");
    body["filter"] = json!({
        "name": "matrix-uhd",
        "atoms": [
            { "kind": "resolution", "value": "2160p", "priority": 100 },
            { "kind": "title_match", "value": "Matrix", "priority": 80 }
        ]
    });
    body
}

async fn create_movie_subscribe(app: &axum::Router) -> String {
    create_site(app).await;
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            movie_subscribe(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn member_cannot_run_another_users_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("search", nexusphp())]),
        }),
        downloader.clone(),
    ));
    let id = create_movie_subscribe(&app).await;
    let (_bob_id, bob) = create_member(&app, "bob").await;

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/run"),
            Some(&bob),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = json_error(response).await;
    assert_eq!(error["code"], "subscription.missing");
    assert!(
        downloader.added().is_empty(),
        "foreign member must not add torrents"
    );
    let stored = Store::open(tmp.path().join("data")).unwrap();
    let subscribe_id = SubscribeId::from_str(&id).unwrap();
    assert!(stored.load_pending(subscribe_id).unwrap().is_empty());
}

#[tokio::test]
async fn paused_subscription_run_does_not_add_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("search", nexusphp())]),
        }),
        downloader.clone(),
    ));
    let id = create_movie_subscribe(&app).await;
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
            &format!("/api/v1/subscriptions/{id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = json_error(response).await;
    assert_eq!(error["code"], "subscription.paused");
    assert!(downloader.added().is_empty());
    let stored = Store::open(tmp.path().join("data")).unwrap();
    assert!(
        stored
            .load_pending(SubscribeId::from_str(&id).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_subscription_run_is_rejected_without_adding() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("search", nexusphp())]),
        }),
        downloader.clone(),
    );
    let app = router(api_state.clone());
    let id = create_movie_subscribe(&app).await;
    let subscribe_id = SubscribeId::from_str(&id).unwrap();
    api_state.mark_subscribe_deleting(subscribe_id);

    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = json_error(response).await;
    assert_eq!(error["code"], "subscription.deleting");
    assert!(downloader.added().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_during_legacy_run_search_skips_downloader_add() {
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
    let api_state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(api_state);
    let id = create_movie_subscribe(&app).await;

    let run_app = app.clone();
    let run_id = id.clone();
    let run = tokio::spawn(async move {
        run_app
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{run_id}/run"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap()
    });
    tokio::task::spawn_blocking(move || {
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("legacy run search started")
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

    let response = run.await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = json_error(response).await;
    assert_eq!(error["code"], "subscription.paused");
    assert!(
        added_torrents.lock().is_empty(),
        "paused Subscribe must not add torrents after a stale search"
    );
    let stored = Store::open(tmp.path().join("data")).unwrap();
    assert!(
        stored
            .load_pending(SubscribeId::from_str(&id).unwrap())
            .unwrap()
            .is_empty()
    );
}
