//! Concurrency races for manual admit and manual downloader submit.
use super::support::*;
use axum::http::StatusCode;
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct SearchPage;
impl Fetcher for SearchPage {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(r#"<html><table class="torrents"><tr class="torrent">
<td><a class="name" title="The.Matrix.1999.1080p.WEB-DL" href="details.php?id=1">x</a></td>
<td><a class="download" href="download.php?id=1&amp;passkey=abc"></a></td>
<td class="size">4.00 GB</td><td class="seeders">42</td><td class="leechers">7</td>
<td class="snatched">180</td><td class="upload_time">2024-01-15 08:30</td>
<td class="category">Movies</td></tr></table></html>"#
            .into())
    }
}

struct GatedRollbackDownloader {
    add_gate: Arc<Gate>,
    added: Arc<AtomicUsize>,
    removed: Arc<AtomicUsize>,
    rollback_guard_free: Arc<AtomicBool>,
    state: Arc<parking_lot::Mutex<Option<api::ApiState>>>,
    subscribe: Arc<parking_lot::Mutex<domain::SubscribeId>>,
}

impl Downloader for GatedRollbackDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        self.add_gate.block();
        self.added.fetch_add(1, Ordering::SeqCst);
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
    fn remove_owned(&self, _: &domain::Torrent, _: bool) -> Result<(), DownloaderError> {
        let sid = *self.subscribe.lock();
        let unlocked = self
            .state
            .lock()
            .as_ref()
            .is_some_and(|s| s.subscribe_guard(sid).try_lock().is_some());
        self.rollback_guard_free.store(unlocked, Ordering::SeqCst);
        self.removed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn race_fixture(
    kind: domain::MediaKind,
) -> (
    Fixture,
    Arc<Gate>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
) {
    let add_gate = Arc::new(Gate::default());
    let added = Arc::new(AtomicUsize::new(0));
    let removed = Arc::new(AtomicUsize::new(0));
    let rollback_free = Arc::new(AtomicBool::new(false));
    let shared_state = Arc::new(parking_lot::Mutex::new(None));
    let shared_sub = Arc::new(parking_lot::Mutex::new(domain::SubscribeId::new()));
    let dl = Arc::new(GatedRollbackDownloader {
        add_gate: add_gate.clone(),
        added: added.clone(),
        removed: removed.clone(),
        rollback_guard_free: rollback_free.clone(),
        state: shared_state.clone(),
        subscribe: shared_sub.clone(),
    });
    let fixture = Fixture::with_fetcher_kind(dl, Arc::new(SearchPage), kind);
    fixture
        .state
        .store()
        .insert_site(&domain::Site {
            id: domain::SiteId::new(),
            name: "Manual search fixture".into(),
            url: "https://pt.example".into(),
            profile_id: "demo".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    *shared_state.lock() = Some(fixture.state.clone());
    *shared_sub.lock() = fixture.subscribe;
    (fixture, add_gate, added, removed, rollback_free)
}

#[test]
fn admit_aborts_with_conflict_and_rolls_back_outside_guard_when_deleted_during_submit() {
    let (fixture, add_gate, added, removed, rollback_free) = race_fixture(domain::MediaKind::Movie);
    let _release = ReleaseOnDrop(add_gate.clone());
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let admit = tokio::spawn(async move {
            use tower::ServiceExt;
            api::router(state)
                .oneshot(
                    axum::http::Request::post("/api/v1/search/admit")
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({
                                "subscribe_id": id.to_string(),
                                "enclosure": "https://pt.example/download.php?id=1&passkey=abc"
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        add_gate.entered().await;
        let del = tokio::time::timeout(
            POSITIVE_SLO,
            fixture.request(
                "DELETE",
                &format!("/api/v1/subscriptions/{id}"),
                Value::Null,
            ),
        )
        .await
        .expect("DELETE must finish while admit is blocked in Downloader::add");
        assert_eq!(del.0, StatusCode::OK, "{}", del.1);
        add_gate.release();

        let resp = admit.await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "subscription.deleting", "{body}");
        assert_eq!(added.load(Ordering::SeqCst), 1);
        assert_eq!(removed.load(Ordering::SeqCst), 1);
        assert!(
            rollback_free.load(Ordering::SeqCst),
            "delivery.rollback must run outside subscribe_guard"
        );
        assert!(fixture.state.store().load_pending(id).unwrap().is_empty());
    });
}

#[test]
fn manual_submit_rolls_back_with_conflict_when_coverage_changes_during_add() {
    let (fixture, add_gate, added, removed, rollback_free) = race_fixture(domain::MediaKind::Tv);
    let _release = ReleaseOnDrop(add_gate.clone());
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let submit = tokio::spawn(async move {
            use tower::ServiceExt;
            api::router(state)
                .oneshot(
                    axum::http::Request::post("/api/v1/downloaders/submit")
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({
                                "download_url": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
                                "title": "The.Matrix.S01E02.1080p.WEB-DL",
                                "subscribe_id": id.to_string(),
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        add_gate.entered().await;
        let patch = tokio::time::timeout(
            POSITIVE_SLO,
            fixture.request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                json!({
                    "coverage": { "kind": "tv", "season": 2, "episode_from": 1, "episode_to": 2 }
                }),
            ),
        )
        .await
        .expect("PATCH coverage must finish while Downloader::add runs");
        assert_eq!(patch.0, StatusCode::OK, "{}", patch.1);
        add_gate.release();

        let resp = submit.await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "subscription.changed", "{body}");
        assert_eq!(added.load(Ordering::SeqCst), 1);
        assert_eq!(removed.load(Ordering::SeqCst), 1);
        assert!(
            rollback_free.load(Ordering::SeqCst),
            "rollback_untracked_delivery must run outside subscribe_guard"
        );
        assert!(fixture.state.store().load_pending(id).unwrap().is_empty());
    });
}

struct ConfigurableGatedDownloader {
    add_gate: Arc<Gate>,
    fail_add: Arc<AtomicBool>,
    added: Arc<AtomicUsize>,
}

impl Downloader for ConfigurableGatedDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        self.add_gate.block();
        self.added.fetch_add(1, Ordering::SeqCst);
        if self.fail_add.load(Ordering::SeqCst) {
            Err(DownloaderError::Message("injected add failure".into()))
        } else {
            Ok(downloader::ClientSubmitResult::Added {
                already_owned: false,
                info_hash: payload.info_hash().to_string(),
            })
        }
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

fn configurable_race_fixture(
    fail_add: bool,
) -> (Fixture, Arc<Gate>, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let add_gate = Arc::new(Gate::default());
    let fail = Arc::new(AtomicBool::new(fail_add));
    let added = Arc::new(AtomicUsize::new(0));
    let dl = Arc::new(ConfigurableGatedDownloader {
        add_gate: add_gate.clone(),
        fail_add: fail.clone(),
        added: added.clone(),
    });
    let fixture = Fixture::with_fetcher_kind(dl, Arc::new(SearchPage), domain::MediaKind::Movie);
    fixture
        .state
        .store()
        .insert_site(&domain::Site {
            id: domain::SiteId::new(),
            name: "Manual search fixture".into(),
            url: "https://pt.example".into(),
            profile_id: "demo".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    (fixture, add_gate, fail, added)
}

#[test]
fn concurrent_identical_enclosure_admit_rejects_second_and_keeps_deterministic_pending_state() {
    for fail_first in [false, true] {
        let (fixture, add_gate, fail_add, added) = configurable_race_fixture(fail_first);
        let _release = ReleaseOnDrop(add_gate.clone());
        runtime().block_on(async {
            let state = fixture.state.clone();
            let id = fixture.subscribe;
            let enclosure = "https://pt.example/download.php?id=1&passkey=abc";
            let first = tokio::spawn(async move {
                use tower::ServiceExt;
                api::router(state)
                    .oneshot(
                        axum::http::Request::post("/api/v1/search/admit")
                            .header("authorization", format!("Bearer {TOKEN}"))
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(
                                json!({ "subscribe_id": id.to_string(), "enclosure": enclosure })
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            });
            add_gate.entered().await;

            let second = tokio::time::timeout(
                POSITIVE_SLO,
                fixture.request(
                    "POST",
                    "/api/v1/search/admit",
                    json!({ "subscribe_id": id.to_string(), "enclosure": enclosure }),
                ),
            )
            .await
            .expect("second concurrent identical admit must not wait behind first");
            assert_eq!(second.0, StatusCode::CONFLICT, "{}", second.1);
            assert_eq!(second.1["error"]["code"], "subscription.admitting");

            add_gate.release();
            let resp = first.await.unwrap();
            let status = resp.status();
            let pending = fixture.state.store().load_pending(id).unwrap();
            if fail_first {
                assert!(!status.is_success(), "expected failure, got {status}");
                assert!(
                    pending.is_empty(),
                    "failed first admit must clean up unconfirmed row"
                );
                // Guard must be released so a subsequent retry can succeed.
                fail_add.store(false, Ordering::SeqCst);
                let retry = fixture
                    .request(
                        "POST",
                        "/api/v1/search/admit",
                        json!({ "subscribe_id": id.to_string(), "enclosure": enclosure }),
                    )
                    .await;
                assert_eq!(retry.0, StatusCode::OK, "{}", retry.1);
                assert_eq!(added.load(Ordering::SeqCst), 2);
            } else {
                assert_eq!(status, StatusCode::OK);
                assert_eq!(pending.len(), 1);
                assert!(pending[0].1.submitted_at.is_some());
                assert_eq!(added.load(Ordering::SeqCst), 1);
            }
        });
    }
}

#[test]
fn admit_rejects_already_confirmed_pending_row_without_overwriting_or_deleting() {
    let (fixture, add_gate, _fail_add, added) = configurable_race_fixture(true);
    add_gate.release();
    let enclosure = "https://pt.example/download.php?id=1&passkey=abc";
    let torrent = domain::Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "The.Matrix.1999.1080p.WEB-DL".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(4 * 1024 * 1024 * 1024),
        seeders: Some(42),
        free: false,
        hr: Some(false),
        imdb_id: None,
        leechers: Some(7),
        snatched: Some(180),
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    fixture
        .state
        .store()
        .record_manual_pending_submissions(
            fixture.subscribe,
            &[(
                100,
                api::store::PendingDownload {
                    submitted_at: Some(12345),
                    torrent,
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();

    runtime().block_on(async {
        let resp = fixture
            .request(
                "POST",
                "/api/v1/search/admit",
                json!({
                    "subscribe_id": fixture.subscribe.to_string(),
                    "enclosure": enclosure,
                }),
            )
            .await;
        assert_eq!(resp.0, StatusCode::CONFLICT, "{}", resp.1);
        assert_eq!(resp.1["error"]["code"], "subscription.already_pending");
        assert_eq!(added.load(Ordering::SeqCst), 0);
        let pending = fixture
            .state
            .store()
            .load_pending(fixture.subscribe)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.submitted_at, Some(12345));
    });
}
