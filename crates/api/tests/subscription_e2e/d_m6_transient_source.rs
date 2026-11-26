//! D-M6: transient source / token-resolution errors must not permanently
//! blacklist enclosures; only definitive source rejections (`SourceRejected` /
//! `IndexerError::Parse`) mark an enclosure rejected and fall back in-round.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use domain::SubscribeId;
use serde_json::json;
use tower::ServiceExt;

use super::common::{json_data, request, subscribe_payload_with_filter};
use super::harness::*;

const ALT_TITLE: &str = "The.Matrix.1999.2160p.BluRay.x265-ALT";

struct StubDownloader {
    transient_outage: AtomicBool,
    reject_first: bool,
    inner: downloader::MemoryDownloader,
}

impl StubDownloader {
    fn new(stage: &Path, transient: bool, reject_first: bool) -> Arc<Self> {
        Arc::new(Self {
            transient_outage: AtomicBool::new(transient),
            reject_first,
            inner: downloader::MemoryDownloader::new(stage),
        })
    }
}

impl downloader::Downloader for StubDownloader {
    fn add(&self, torrent: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        self.add_with_options(torrent, &torrent.enclosure, None)
    }

    fn add_with_options(
        &self,
        torrent: &domain::Torrent,
        download_url: &str,
        save_path: Option<&str>,
    ) -> Result<(), downloader::DownloaderError> {
        if self.transient_outage.load(Ordering::SeqCst) {
            return Err(downloader::DownloaderError::Message("tracker 503".into()));
        }
        if self.reject_first && torrent.enclosure.contains("id=1") {
            return Err(downloader::DownloaderError::SourceRejected(
                "404 not found".into(),
            ));
        }
        self.inner
            .add_with_options(torrent, download_url, save_path)
    }

    fn completed_files(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<Vec<PathBuf>, downloader::DownloaderError> {
        self.inner.completed_files(torrent)
    }

    fn owned_identity(
        &self,
        torrent: &domain::Torrent,
    ) -> Result<Option<String>, downloader::DownloaderError> {
        self.inner.owned_identity(torrent)
    }

    fn endpoint(&self) -> Option<String> {
        self.inner.endpoint()
    }
}

async fn prefer_group_filter(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some(TOKEN),
            json!({
                "name": "prefer-group",
                "atoms": [
                    { "kind": "team", "value": "GROUP", "priority": 120 },
                    { "kind": "resolution", "value": "2160p", "priority": 100 }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn transient_errors_across_candidates_do_not_blacklist_sources_and_retry_next_round() {
    let stage = tempfile::tempdir().unwrap();
    let stub = StubDownloader::new(stage.path(), true, false);
    let h = Harness::with_downloader(stub.clone());
    h.create_site().await;
    let filter_id = prefer_group_filter(&h.app).await;
    let id = h
        .create_movie_subscribe(json!({ "filter_id": filter_id }))
        .await;
    h.set_search_rows(&[(MATRIX_TITLE, 1), (ALT_TITLE, 2)]);

    let err = h.scheduled_search(id).await.unwrap_err();
    assert!(
        err.contains("tracker 503"),
        "expected transient error: {err}"
    );
    assert!(
        h.state
            .store()
            .load_rejected_enclosures(id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.pending_state(id, "rejected"), 0);
    assert_eq!(h.pending_state(id, "active"), 0);

    stub.transient_outage.store(false, Ordering::SeqCst);
    h.scheduled_search(id).await.unwrap();
    assert!(
        h.state
            .store()
            .load_rejected_enclosures(id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.pending_state(id, "rejected"), 0);
    assert_eq!(h.pending_state(id, "active"), 1);
    let active: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(active, vec![h.enclosure(1)]);
}

#[tokio::test]
async fn definitive_source_rejected_blacklists_first_candidate_and_falls_back_to_second() {
    let stage = tempfile::tempdir().unwrap();
    let stub = StubDownloader::new(stage.path(), false, true);
    let h = Harness::with_downloader(stub);
    h.create_site().await;
    let filter_id = prefer_group_filter(&h.app).await;
    let id = h
        .create_movie_subscribe(json!({ "filter_id": filter_id }))
        .await;
    h.set_search_rows(&[(MATRIX_TITLE, 1), (ALT_TITLE, 2)]);

    h.scheduled_search(id).await.unwrap();
    let rejected = h.state.store().load_rejected_enclosures(id).unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(rejected.contains(&h.enclosure(1)));
    assert_eq!(h.pending_state(id, "rejected"), 1);
    assert_eq!(h.pending_state(id, "active"), 1);
    let active: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(active, vec![h.enclosure(2)]);
}

struct MteamTokenFixture {
    transient_fetch: AtomicBool,
}

impl indexer::Fetcher for MteamTokenFixture {
    fn fetch(&self, req: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        if req.key.starts_with("search:") {
            return Ok(json!({
                "code": "0",
                "message": "SUCCESS",
                "data": {
                    "data": [
                        {
                            "id": "1",
                            "name": MATRIX_TITLE,
                            "size": "4 GB",
                            "status": { "seeders": "42" }
                        },
                        {
                            "id": "2",
                            "name": ALT_TITLE,
                            "size": "4 GB",
                            "status": { "seeders": "20" }
                        }
                    ]
                }
            })
            .to_string());
        }
        if self.transient_fetch.load(Ordering::SeqCst) {
            return Err(indexer::IndexerError::Fetch("503 gateway timeout".into()));
        }
        if req.key.ends_with(":1") {
            return Ok(r#"{"code":"404","message":"deleted","data":null}"#.into());
        }
        Ok(
            r#"{"code":"0","message":"SUCCESS","data":"https://dl.example.invalid/2.torrent"}"#
                .into(),
        )
    }
}

async fn run_search_job(state: &api::ApiState, id: SubscribeId) -> Result<(), String> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        use jobs::Runner;
        let payload = json!({ "subscribe_id": id.to_string() }).to_string();
        let job = state
            .jobs()
            .acquire()
            .unwrap()
            .enqueue(jobs::JobIdentity::subscribe_search(id), &payload, 0)
            .unwrap();
        api::worker::StateRunner {
            state: state.clone(),
        }
        .run(&job)
    })
    .await
    .unwrap()
}

async fn seed_mteam_subscribe(app: &axum::Router) -> SubscribeId {
    let site_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            json!({
                "name": "mteam",
                "url": "https://pt.example.invalid",
                "profile_id": "mteam",
                "api_key": "secret-key",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(site_resp.status(), StatusCode::CREATED);

    let filter_id = prefer_group_filter(app).await;
    let sub_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(TOKEN),
            subscribe_payload_with_filter("search", Some(&filter_id)),
        ))
        .await
        .unwrap();
    assert_eq!(sub_resp.status(), StatusCode::CREATED);
    json_data(sub_resp).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn indexer_resolve_fetch_error_retries_while_parse_error_falls_back() {
    install_fake_ffprobe();
    let tmp = tempfile::tempdir().unwrap();
    let fixture = Arc::new(MteamTokenFixture {
        transient_fetch: AtomicBool::new(true),
    });
    let downloader = Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage")));
    let state = api::ApiState::new(
        api::Store::open(tmp.path().join("data")).unwrap(),
        TOKEN.into(),
        indexer::ProfileSet::load(None).unwrap(),
        fixture.clone(),
        downloader,
    )
    .unwrap();
    let app = api::router(state.clone());
    let id = seed_mteam_subscribe(&app).await;

    let err = run_search_job(&state, id).await.unwrap_err();
    assert!(
        err.contains("503 gateway timeout"),
        "expected transient fetch error: {err}"
    );
    assert!(
        state
            .store()
            .load_rejected_enclosures(id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state
            .store()
            .load_pending_state(id, "rejected")
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        state
            .store()
            .load_pending_state(id, "active")
            .unwrap()
            .len(),
        0
    );

    fixture.transient_fetch.store(false, Ordering::SeqCst);
    run_search_job(&state, id).await.unwrap();
    let rejected = state.store().load_rejected_enclosures(id).unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(rejected.contains("https://pt.example.invalid/api/torrent/download?id=1"));
    let active: Vec<_> = state
        .store()
        .load_pending(id)
        .unwrap()
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(
        active,
        vec!["https://pt.example.invalid/api/torrent/download?id=2".to_string()]
    );
}
