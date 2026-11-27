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
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        if self.block_add {
            self.gate.block();
        }
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
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
            browser_search: false,
            timezone: "UTC".into(),
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
        let detail =
            tokio::time::timeout(POSITIVE_SLO, fixture.request("GET", &uri, Value::Null)).await;
        let mutation = tokio::time::timeout(
            POSITIVE_SLO,
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
            POSITIVE_SLO,
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
        .acquire()
        .unwrap()
        .enqueue(
            jobs::JobIdentity::subscribe_search(id),
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
            POSITIVE_SLO,
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
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        if self.tasks.lock().is_empty() {
            self.gate.block();
        }
        self.tasks.lock().push(domain::Torrent {
            site_id: domain::SiteId::new(),
            title: options.ownership_tag.clone(),
            enclosure: payload.info_hash().to_string(),
            size_bytes: None,
            seeders: None,
            free: false,
            hr: None,
            imdb_id: None,
            id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
            free_2x: false,
            free_until: None,
        });
        Ok(downloader::ClientSubmitResult::Added {
            already_owned: false,
            info_hash: payload.info_hash().to_string(),
        })
    }
    fn remember_identity(
        &self,
        torrent: &domain::Torrent,
        _hash: &str,
    ) -> Result<(), DownloaderError> {
        if let Some(last) = self.tasks.lock().last_mut() {
            last.enclosure = torrent.enclosure.clone();
        }
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
                .acquire()
                .unwrap()
                .enqueue(
                    jobs::JobIdentity::subscribe_search(id),
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

struct GatedSearchFetcher(Arc<Gate>);
impl Fetcher for GatedSearchFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        self.0.block();
        SearchPage.fetch(req)
    }
}

#[test]
fn admit_releases_subscribe_guard_during_search_and_downloader_submit() {
    let search_gate = Arc::new(Gate::default());
    let f1 = configured_fixture(
        Arc::new(ManualDownloader {
            gate: Arc::new(Gate::default()),
            block_add: false,
        }),
        Arc::new(GatedSearchFetcher(search_gate.clone())),
    );
    let _r1 = ReleaseOnDrop(search_gate.clone());
    runtime().block_on(async {
        let state = f1.state.clone();
        let id = f1.subscribe;
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
        search_gate.entered().await;
        let uri = format!("/api/v1/subscriptions/{id}");
        let patched = tokio::time::timeout(
            POSITIVE_SLO,
            f1.request("PATCH", &uri, json!({ "tracking_state": "paused" })),
        )
        .await
        .expect("PATCH must finish while admit is blocked in PT search");
        assert_eq!(patched.0, StatusCode::OK, "{}", patched.1);
        search_gate.release();
        let resp = admit.await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    });

    // Also prove admit releases subscribe_guard during Downloader::add and aborts if deleted during search.
    let add_gate = Arc::new(Gate::default());
    let f2 = fixture(add_gate.clone(), true);
    let _r2 = ReleaseOnDrop(add_gate.clone());
    runtime().block_on(async {
        let state = f2.state.clone();
        let id = f2.subscribe;
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
        let uri = format!("/api/v1/subscriptions/{id}");
        let patched = tokio::time::timeout(
            POSITIVE_SLO,
            f2.request("PATCH", &uri, json!({ "tracking_state": "paused" })),
        )
        .await
        .expect("PATCH must finish while admit is blocked in Downloader::add");
        assert_eq!(patched.0, StatusCode::OK, "{}", patched.1);
        add_gate.release();
        assert_eq!(admit.await.unwrap().status(), StatusCode::OK);
    });
}

struct GatedFileStats {
    gate: Arc<Gate>,
    blocked_once: std::sync::atomic::AtomicBool,
}
impl api::file_stat_read::FileStatRead for GatedFileStats {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        if !self
            .blocked_once
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.gate.block();
        }
        path.is_file()
    }
}

struct RollbackCheckDownloader {
    add_gate: Arc<Gate>,
    rollback_guard_free: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<parking_lot::Mutex<Option<api::ApiState>>>,
    subscribe: Arc<parking_lot::Mutex<domain::SubscribeId>>,
}
impl Downloader for RollbackCheckDownloader {
    fn submit_verified(
        &self,
        payload: &downloader::SubmitPayload,
        _options: &downloader::SubmitOptions,
    ) -> Result<downloader::ClientSubmitResult, DownloaderError> {
        self.add_gate.block();
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
        self.rollback_guard_free
            .store(unlocked, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

fn seed_tv_submit_fixture(
    tmp: &std::path::Path,
    dl: Arc<dyn Downloader>,
    fs_gate: Arc<Gate>,
) -> (api::ApiState, domain::SubscribeId) {
    let state = api::ApiState::new(
        api::Store::open(tmp.join("data2")).unwrap(),
        TOKEN.into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(SearchPage),
        dl,
    )
    .unwrap()
    .with_file_stat_read(Arc::new(GatedFileStats {
        gate: fs_gate,
        blocked_once: std::sync::atomic::AtomicBool::new(false),
    }));
    let store = state.store();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
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
    let filter = domain::Filter::new(domain::FilterId::new(), "Unrestricted", vec![]);
    store.insert_filter(&filter).unwrap();
    let sub = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: store.user_by_token(TOKEN).unwrap().unwrap().id,
        media_id: media.id,
        coverage: domain::Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: domain::FetchMode::Search,
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
    store.insert_subscribe(&sub).unwrap();
    let existing = tmp.join("data2/library/tv/s01e01.mkv");
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    std::fs::write(&existing, b"v").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: existing.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    (state, sub.id)
}

#[test]
fn manual_submit_releases_subscribe_guard_during_fs_stats_add_and_rollback() {
    let fs_gate = Arc::new(Gate::default());
    let add_gate = Arc::new(Gate::default());
    let rollback_free = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let shared_state = Arc::new(parking_lot::Mutex::new(None));
    let shared_sub = Arc::new(parking_lot::Mutex::new(domain::SubscribeId::new()));
    let dl = Arc::new(RollbackCheckDownloader {
        add_gate: add_gate.clone(),
        rollback_guard_free: rollback_free.clone(),
        state: shared_state.clone(),
        subscribe: shared_sub.clone(),
    });
    let (state, sub_id) = seed_tv_submit_fixture(tmp.path(), dl, fs_gate.clone());
    *shared_state.lock() = Some(state.clone());
    *shared_sub.lock() = sub_id;
    let _r_fs = ReleaseOnDrop(fs_gate.clone());
    let _r_add = ReleaseOnDrop(add_gate.clone());
    runtime().block_on(async {
        let submit_state = state.clone();
        let submit = tokio::spawn(async move {
            use tower::ServiceExt;
            api::router(submit_state)
                .oneshot(
                    axum::http::Request::post("/api/v1/downloaders/submit")
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({
                                "download_url": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
                                "title": "The.Matrix.S01E02.1080p.WEB-DL",
                                "subscribe_id": sub_id.to_string(),
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        let patch_req = |body: Value| {
            let state = state.clone();
            async move {
                use tower::ServiceExt;
                api::router(state)
                    .oneshot(
                        axum::http::Request::builder()
                            .method("PATCH")
                            .uri(format!("/api/v1/subscriptions/{sub_id}"))
                            .header("authorization", format!("Bearer {TOKEN}"))
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };
        fs_gate.entered().await;
        let p1 = tokio::time::timeout(POSITIVE_SLO, patch_req(json!({"follow_future": true}))).await;
        assert_eq!(p1.expect("PATCH must finish during FileStatRead::is_file").status(), StatusCode::OK);
        fs_gate.release();

        add_gate.entered().await;
        let p2 = tokio::time::timeout(POSITIVE_SLO, patch_req(json!({"tracking_state": "paused"}))).await;
        assert_eq!(p2.expect("PATCH must finish during Downloader::add").status(), StatusCode::OK);
        let del = tokio::time::timeout(POSITIVE_SLO, async {
            use tower::ServiceExt;
            api::router(state.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .method("DELETE")
                        .uri(format!("/api/v1/subscriptions/{sub_id}"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        })
        .await;
        assert_eq!(del.expect("DELETE must finish while Downloader::add runs").status(), StatusCode::OK);
        add_gate.release();
        let resp = submit.await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(rollback_free.load(std::sync::atomic::Ordering::SeqCst), "remove_owned must run outside subscribe_guard");
        assert!(state.store().load_pending(sub_id).unwrap().is_empty());
    });
}
