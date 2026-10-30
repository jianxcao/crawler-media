//! Manual HTTP admission must release the Subscribe guard around Downloader IO.
use super::support::*;
use axum::http::StatusCode;
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

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

struct ManualDownloader {
    gate: Arc<Gate>,
    block_add: bool,
}
impl Downloader for ManualDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), DownloaderError> {
        if self.block_add {
            self.gate.block();
        }
        Ok(())
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        if !self.block_add {
            self.gate.block();
        }
        Ok(Vec::new())
    }
}

fn fixture(gate: Arc<Gate>, block_add: bool) -> Fixture {
    configured_fixture(
        Arc::new(ManualDownloader { gate, block_add }),
        Arc::new(SearchPage),
    )
}

fn configured_fixture<F: Fetcher + 'static>(
    downloader: Arc<dyn Downloader>,
    fetcher: Arc<F>,
) -> Fixture {
    with_site(Fixture::with_fetcher(downloader, fetcher))
}

fn with_site(fixture: Fixture) -> Fixture {
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
        })
        .unwrap();
    fixture
}

#[test]
fn manual_run_releases_subscribe_guard_during_downloader_io() {
    let gate = Arc::new(Gate::default());
    let fixture = fixture(gate.clone(), false);
    let _release = ReleaseOnDrop(gate.clone());
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let manual = tokio::spawn(async move {
            use tower::ServiceExt;
            api::router(state)
                .oneshot(
                    axum::http::Request::post(format!("/api/v1/subscriptions/{id}/run"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        gate.entered().await;
        let uri = format!("/api/v1/subscriptions/{id}");
        let detail = tokio::time::timeout(SLO, fixture.request("GET", &uri, Value::Null)).await;
        let mutation = tokio::time::timeout(
            SLO,
            fixture.request("PATCH", &uri, json!({"tracking_state":"paused"})),
        )
        .await;
        gate.release();
        let response = manual.await.unwrap();
        let (status, body) =
            detail.expect("same Subscribe read must finish during manual Downloader IO");
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) =
            mutation.expect("same Subscribe PATCH must finish during manual Downloader IO");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            fixture
                .state
                .store()
                .get_subscribe(id)
                .unwrap()
                .unwrap()
                .tracking_state,
            "paused"
        );
        assert!(
            !response.status().is_success(),
            "manual snapshot paused during IO must be refused"
        );
        let store = fixture.state.store();
        let pending = store.load_pending(id).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "the accepted external task must remain tracked after pause"
        );
        assert_eq!(
            pending[0].1.torrent.enclosure,
            "https://pt.example/download.php?id=1&passkey=abc"
        );
        assert!(
            pending[0]
                .1
                .submitted_at
                .is_some_and(|submitted| submitted > 0)
        );
        assert!(
            store
                .load_subscribe_facts(id)
                .unwrap()
                .entries()
                .next()
                .is_none(),
            "stale manual result must not publish facts"
        );
        assert!(
            store
                .file_coverage_for_media(fixture.media)
                .unwrap()
                .is_empty(),
            "stale manual result must not publish Library files"
        );
    });
}

#[test]
fn manual_run_records_successful_add_after_pause_without_publishing_stale_files() {
    let gate = Arc::new(Gate::default());
    let fixture = fixture(gate.clone(), true);
    let _release = ReleaseOnDrop(gate.clone());
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let manual = tokio::spawn(async move {
            use tower::ServiceExt;
            api::router(state)
                .oneshot(
                    axum::http::Request::post(format!("/api/v1/subscriptions/{id}/run"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        gate.entered().await;
        // An unconfirmed remote call must not appear as an accepted submission.
        let intent = fixture.state.store().load_pending(id).unwrap();
        let mutation = tokio::time::timeout(
            SLO,
            fixture.request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                json!({"tracking_state":"paused"}),
            ),
        )
        .await;
        gate.release();
        let response = manual.await.unwrap();
        assert!(
            intent.is_empty(),
            "unfinished add is not a confirmed submission"
        );
        let (status, body) = mutation.expect("pause must finish while remote add is blocked");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!response.status().is_success());
        let store = fixture.state.store();
        let pending = store.load_pending(id).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "successful add remains tracked even after pause"
        );
        assert!(
            pending[0]
                .1
                .submitted_at
                .is_some_and(|submitted| submitted > 0)
        );
        assert_eq!(
            store.get_subscribe(id).unwrap().unwrap().tracking_state,
            "paused"
        );
        assert!(
            store
                .file_coverage_for_media(fixture.media)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn scheduled_search_releases_subscribe_guard_during_add_and_tracks_success_after_pause() {
    let gate = Arc::new(Gate::default());
    let fixture = fixture(gate.clone(), true);
    let _release = ReleaseOnDrop(gate.clone());
    let id = fixture.subscribe;
    let job = fixture
        .state
        .jobs()
        .lock()
        .enqueue(
            jobs::JobKind::SubscribeSearch,
            &json!({"subscribe_id": id}).to_string(),
            0,
        )
        .unwrap();
    runtime().block_on(async {
        let state = fixture.state.clone();
        let search = tokio::task::spawn_blocking(move || {
            use jobs::Runner;
            api::worker::StateRunner { state }.run(&job)
        });
        gate.entered().await;
        let mutation = tokio::time::timeout(
            SLO,
            fixture.request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                json!({"tracking_state":"paused"}),
            ),
        )
        .await;
        gate.release();
        let _outcome = search.await.unwrap();
        let (status, body) =
            mutation.expect("scheduled add must release the Subscribe guard for PATCH");
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = fixture.state.store();
        assert_eq!(
            store.get_subscribe(id).unwrap().unwrap().tracking_state,
            "paused"
        );
        let pending = store.load_pending(id).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "accepted external task must remain tracked after pause"
        );
        assert_eq!(
            pending[0].1.torrent.enclosure,
            "https://pt.example/download.php?id=1&passkey=abc"
        );
        assert!(
            store
                .file_coverage_for_media(fixture.media)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .load_subscribe_facts(id)
                .unwrap()
                .entries()
                .next()
                .is_none()
        );
    });
}

struct TwoEpisodesPage;
impl Fetcher for TwoEpisodesPage {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let page = SearchPage.fetch(request)?;
        let episode = page.replace(
            "The.Matrix.1999.1080p.WEB-DL",
            "The.Matrix.S01E01.1080p.WEB-DL",
        );
        let start = episode.find("<tr ").unwrap();
        let end = episode.find("</tr>").unwrap() + "</tr>".len();
        let second = episode[start..end]
            .replace("S01E01", "S01E02")
            .replace("id=1", "id=2");
        Ok(episode.replace("</table>", &format!("{second}</table>")))
    }
}

struct GatedTaskList {
    gate: Arc<Gate>,
    tasks: Arc<parking_lot::Mutex<Vec<domain::Torrent>>>,
}
impl Downloader for GatedTaskList {
    fn add(&self, torrent: &domain::Torrent) -> Result<(), DownloaderError> {
        if self.tasks.lock().is_empty() {
            self.gate.block();
        }
        self.tasks.lock().push(torrent.clone());
        Ok(())
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

fn assert_cancelled_library_stops_further_adds(manual: bool) {
    let gate = Arc::new(Gate::default());
    let tasks = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let fixture = with_site(Fixture::with_fetcher_kind(
        Arc::new(GatedTaskList {
            gate: gate.clone(),
            tasks: tasks.clone(),
        }),
        Arc::new(TwoEpisodesPage),
        domain::MediaKind::Tv,
    ));
    let _release = ReleaseOnDrop(gate.clone());
    let store = fixture.state.store();
    let library = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap();
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let mut run = tokio::spawn(async move {
            if manual {
                use tower::ServiceExt;
                let response = api::router(state)
                    .oneshot(
                        axum::http::Request::post(format!("/api/v1/subscriptions/{id}/run"))
                            .header("authorization", format!("Bearer {TOKEN}"))
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                return format!("{status}: {}", String::from_utf8_lossy(&body));
            }
            let job = state
                .jobs()
                .lock()
                .enqueue(
                    jobs::JobKind::SubscribeSearch,
                    &json!({"subscribe_id": id}).to_string(),
                    0,
                )
                .unwrap();
            tokio::task::spawn_blocking(move || {
                use jobs::Runner;
                format!("{:?}", api::worker::StateRunner { state }.run(&job))
            })
            .await
            .unwrap()
        });
        tokio::select! {
            _ = gate.entered() => {},
            outcome = &mut run => panic!("run ended before the first external add: {outcome:?}"),
        }
        fixture.state.library_work().cancel(&library.id);
        gate.release();
        let _refused = run.await.unwrap();
        let external: Vec<_> = tasks
            .lock()
            .iter()
            .map(|torrent| torrent.enclosure.clone())
            .collect();
        assert_eq!(
            external,
            vec!["https://pt.example/download.php?id=1&passkey=abc"],
            "Library cancellation must leave only the already accepted external task"
        );
        assert_eq!(
            store.load_pending(id).unwrap().len(),
            1,
            "the first accepted external task must stay tracked"
        );
        assert!(
            store
                .file_coverage_for_media(fixture.media)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn manual_run_stops_further_adds_when_library_is_cancelled() {
    assert_cancelled_library_stops_further_adds(true);
}

#[test]
fn scheduled_search_stops_further_adds_when_library_is_cancelled() {
    assert_cancelled_library_stops_further_adds(false);
}
