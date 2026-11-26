#[path = "management/common.rs"]
mod common;
#[path = "support/theintrodb.rs"]
mod fixture;

use axum::http::StatusCode;
use domain::{FileCoverage, Media, MediaId, MediaKind, UnitKey};
use fixture::Server;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use tower::ServiceExt;

const RESPONSE: &str = r#"{"tvdb_id":169,"type":"tv","season":1,"episode":__EP__,"group_duration_ms":2700000,"intro":[{"start_ms":null,"end_ms":70500,"confidence":0.87,"submission_count":12}]}"#;

struct Clock(AtomicI64);
impl api::playback_events::PlaybackClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct NoMediaReads;
impl api::marker_resolver::ChapterProbe for NoMediaReads {
    fn probe(&self, _: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        panic!("cloud marker rate-limit tests must not read STRM sources")
    }
}

struct LocalClient(String);
impl api::theintrodb_lookup::ClientFactory for LocalClient {
    fn create(&self, key: Option<String>) -> api::theintrodb::TheIntroDbClient {
        api::theintrodb::TheIntroDbClient::new(key).with_base_url(&self.0)
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Arc<api::Store>,
    state: api::ApiState,
    clock: Arc<Clock>,
    media: Media,
    files: Vec<(UnitKey, FileCoverage)>,
}

fn setup_episodes(server: &Server, durations: &[Option<i64>]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(api::Store::open(tmp.path().join("data")).unwrap());
    let clock = Arc::new(Clock(AtomicI64::new(10_000)));
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Rate Limit Show".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("123".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let root = tmp.path().join("strm");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "Rate Limit",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib.id, true, false)
        .unwrap();
    let mut files = Vec::new();
    for (idx, duration) in durations.iter().enumerate() {
        let ep = (idx as u32) + 2;
        let path = root.join(format!("S01E{ep:02}.strm"));
        std::fs::write(&path, format!("https://fixture.invalid/e{ep}.mkv")).unwrap();
        let file = store::test_support::publish_fixture_file_units(
            &store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            &[UnitKey::new(1, ep as i32)],
        )
        .unwrap();
        if duration.is_some() {
            store
                .publish_probe_result(
                    file.file_id,
                    &store::library_write::probe::ProbePublication {
                        expected: &file.version,
                        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                        resolution: None,
                        codec: None,
                        hdr: store::library_write::probe::Observed::NotObserved,
                        duration_ms: *duration,
                        details: None,
                    },
                    0,
                )
                .unwrap();
        }
        store
            .put_cached_chapters(&file.file_id.to_string(), &[])
            .unwrap();
        let cov = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        files.push((UnitKey::new(1, ep as i32), cov));
    }
    store
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true,"theintrodb_api_key":"key-a"}"#)
        .unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_playback_clock(clock.clone())
    .with_chapter_probe(Arc::new(NoMediaReads))
    .with_theintrodb_factory(Arc::new(LocalClient(server.url.clone())));
    Fixture {
        _tmp: tmp,
        store,
        state,
        clock,
        media,
        files,
    }
}

async fn get_chapters(f: &Fixture, idx: usize) -> Vec<serde_json::Value> {
    let (unit, cov) = &f.files[idx];
    let url = format!(
        "/api/v1/libraries/{}/items/{}/chapters?file_id={}&season={}&episode={}",
        cov.file.library_id, f.media.id, cov.file.file_id, unit.season, unit.episode
    );
    let resp = api::router(f.state.clone())
        .oneshot(common::request(
            "GET",
            &url,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    common::json_data(resp).await.as_array().unwrap().clone()
}

async fn open_session_pending(f: &Fixture, idx: usize) -> bool {
    let (unit, cov) = &f.files[idx];
    let resp = api::router(f.state.clone())
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            serde_json::json!({
                "media_item_id": f.media.id,
                "file_id": cov.file.file_id,
                "season_number": unit.season,
                "episode_number": unit.episode,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    common::json_data(resp).await["chapters_pending"]
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn daily_quota_429_trips_cooldown_skips_session_lookup_and_recovers_on_key_change_or_clock() {
    let server = Server::for_any_episode(
        429,
        &[
            ("X-UsageLimit-Reset", "3600"),
            ("X-UsageLimit-Remaining", "0"),
        ],
        r#"{"error":"Usage limit exceeded","code":"usage_limit_exceeded"}"#,
        true,
    );
    let f = setup_episodes(&server, &[Some(2_700_000), Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Within cooldown: session reports chapters_pending=false and other episode skips HTTP.
    assert!(!open_session_pending(&f, 1).await);
    assert!(get_chapters(&f, 1).await.is_empty());
    f.clock.0.store(10_000 + 3_500_000, Ordering::SeqCst);
    assert!(get_chapters(&f, 1).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Switching API key bumps configuration_revision -> new scope recovers immediately.
    server.respond_with_headers(200, &[], RESPONSE);
    f.store
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true,"theintrodb_api_key":"key-b"}"#)
        .unwrap();
    let chapters = get_chapters(&f, 1).await;
    assert!(chapters.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);

    // Trigger DailyQuota again on new scope and verify natural clock expiry restores access.
    server.respond_with_headers(
        429,
        &[
            ("X-UsageLimit-Reset", "3600"),
            ("X-UsageLimit-Remaining", "0"),
        ],
        r#"{"error":"Usage limit exceeded","code":"usage_limit_exceeded"}"#,
    );
    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);

    server.respond_with_headers(200, &[], RESPONSE);
    f.clock
        .0
        .store(10_000 + 3_500_000 + 3_600_001, Ordering::SeqCst);
    let recovered = get_chapters(&f, 0).await;
    assert!(recovered.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn burst_429_uses_short_x_ratelimit_reset_window() {
    let server = Server::for_any_episode(
        429,
        &[("X-RateLimit-Reset", "9")],
        r#"{"error":"Too Many Requests"}"#,
        true,
    );
    let f = setup_episodes(&server, &[Some(2_700_000), Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    server.respond_with_headers(200, &[], RESPONSE);
    f.clock.0.store(10_000 + 5_000, Ordering::SeqCst);
    assert!(!open_session_pending(&f, 1).await);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    f.clock.0.store(10_000 + 9_001, Ordering::SeqCst);
    assert!(open_session_pending(&f, 1).await);
    let (ep3_unit, ep3_cov) = &f.files[1];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while f
        .store
        .get_file_marker(ep3_cov.file.file_id, *ep3_unit)
        .unwrap()
        .is_none()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn specific_media_429_and_rejected_candidates_cache_per_file_without_blocking_other_episodes()
{
    let server = Server::for_any_episode(
        429,
        &[("X-UsageLimit-SpecificMedia-Reset", "1800")],
        r#"{"error":"Specific media cap","code":"specific_media_usage_limit_exceeded"}"#,
        true,
    );
    // Ep 2: 2700s (gets specific_media 429), Ep 3: None (unknown duration), Ep 4: 2700s (succeeds)
    let mut f = setup_episodes(&server, &[Some(2_700_000), None, Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    // Repeated check for Ep 2 hits per-file cache -> no second request.
    assert!(!open_session_pending(&f, 0).await);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Ep 4 is NOT blocked by Ep 2's SpecificMedia 429!
    server.respond_with_headers(200, &[], RESPONSE);
    let ep4 = get_chapters(&f, 2).await;
    assert!(ep4.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);

    // Ep 3 (unknown duration): skips cloud lookup completely without making HTTP requests.
    assert!(!open_session_pending(&f, 1).await);
    assert!(get_chapters(&f, 1).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    assert!(get_chapters(&f, 1).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);

    // Once probe populates duration_ms for Ep 3, cloud lookup runs and adopts IntroStart!
    let ep3_cov = f.files[1].1.clone();
    f.store
        .publish_probe_result(
            ep3_cov.file.file_id,
            &store::library_write::probe::ProbePublication {
                expected: &ep3_cov.file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: Some(2_700_000),
                details: None,
            },
            0,
        )
        .unwrap();
    f.files[1].1 = f
        .store
        .file_coverage_by_id(ep3_cov.file.file_id)
        .unwrap()
        .unwrap();
    let ep3 = get_chapters(&f, 1).await;
    assert!(ep3.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn unauthorized_401_cools_down_scope_for_one_hour_until_key_updated() {
    let server = Server::for_any_episode(401, &[], r#"{"error":"Unauthorized"}"#, true);
    let f = setup_episodes(&server, &[Some(2_700_000), Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    assert!(!open_session_pending(&f, 1).await);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    f.clock.0.store(10_000 + 3_599_000, Ordering::SeqCst);
    assert!(!open_session_pending(&f, 1).await);

    f.clock.0.store(10_000 + 3_600_001, Ordering::SeqCst);
    assert!(open_session_pending(&f, 1).await);
}

#[tokio::test]
async fn burst_429_with_informational_usage_headers_is_not_a_daily_cooldown() {
    let server = Server::for_any_episode(
        429,
        &[
            ("X-UsageLimit-Remaining", "42"),
            ("X-UsageLimit-Reset", "86400"),
            ("X-RateLimit-Reset", "5"),
        ],
        r#"{"error":"Too Many Requests"}"#,
        true,
    );
    let f = setup_episodes(&server, &[Some(2_700_000), Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    server.respond_with_headers(200, &[], RESPONSE);
    f.clock.0.store(10_000 + 4_000, Ordering::SeqCst);
    assert!(!open_session_pending(&f, 1).await);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Recovers right after 5s Burst window, NOT blocked for 86400s DailyQuota.
    f.clock.0.store(10_000 + 5_001, Ordering::SeqCst);
    let chapters = get_chapters(&f, 1).await;
    assert!(chapters.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unrelated_configuration_revision_change_keeps_daily_cooldown() {
    let server = Server::for_any_episode(
        429,
        &[
            ("X-UsageLimit-Reset", "3600"),
            ("X-UsageLimit-Remaining", "0"),
        ],
        r#"{"error":"Usage limit exceeded","code":"usage_limit_exceeded"}"#,
        true,
    );
    let f = setup_episodes(&server, &[Some(2_700_000), Some(2_700_000)]);

    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Unrelated scrape settings update bumps configuration_revision while keeping key-a:
    // active DailyQuota cooldown must stay in effect.
    server.respond_with_headers(200, &[], RESPONSE);
    let rev_before = f.store.raw_scrape_settings().unwrap().revision;
    f.store
        .save_raw_scrape_settings(
            r#"{"theintrodb_enabled":true,"theintrodb_api_key":"key-a","tmdb_language":"en-US"}"#,
        )
        .unwrap();
    assert!(f.store.raw_scrape_settings().unwrap().revision > rev_before);
    assert!(!open_session_pending(&f, 1).await);
    assert!(get_chapters(&f, 1).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // Changing theintrodb_api_key switches cooldown_scope and recovers immediately.
    f.store
        .save_raw_scrape_settings(
            r#"{"theintrodb_enabled":true,"theintrodb_api_key":"key-b","tmdb_language":"en-US"}"#,
        )
        .unwrap();
    let chapters = get_chapters(&f, 1).await;
    assert!(chapters.iter().any(|c| c["marker_type"] == "IntroStart"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unknown_duration_skips_cloud_lookup_without_network() {
    let server = Server::for_any_episode(200, &[], RESPONSE, true);
    let f = setup_episodes(&server, &[None]);

    assert!(!open_session_pending(&f, 0).await);
    assert!(get_chapters(&f, 0).await.is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_chapters_get_returns_without_blocking_on_library_writer_lock() {
    let server = Server::for_any_episode(
        404,
        &[],
        r#"{"error":"No accepted submissions","code":"no_submissions"}"#,
        true,
    );
    let mut f = setup_episodes(&server, &[None, Some(2_700_000), Some(2_700_000)]);
    let part_one = [library::ChapterMarker {
        start_ms: 0,
        end_ms: 600_000,
        title: Some("Part 1".into()),
        marker_type: None,
        synthetic: false,
    }];
    for (_, cov) in &f.files {
        f.store
            .put_cached_chapters(&cov.file.file_id.to_string(), &part_one)
            .unwrap();
    }
    let state_store = api::Store::open(f._tmp.path().join("data")).unwrap();
    let writers = state_store.writers();
    f.state = api::ApiState::new(
        state_store,
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(
            f._tmp.path().join("stage2"),
        )),
    )
    .unwrap()
    .with_playback_clock(f.clock.clone())
    .with_chapter_probe(Arc::new(NoMediaReads))
    .with_theintrodb_factory(Arc::new(LocalClient(server.url.clone())));

    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer_holder = std::thread::spawn(move || {
        writers
            .with_writer_timeout(
                std::time::Duration::from_secs(10),
                store::Db::Library,
                |_conn| {
                    locked_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                    Ok(())
                },
            )
            .unwrap();
    });
    locked_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();

    let fast = std::time::Duration::from_millis(800);
    // (a) Unknown duration_ms = None with cached_chapters = Some([Part 1])
    let unknown = tokio::time::timeout(fast, get_chapters(&f, 0))
        .await
        .expect("unknown-duration GET chapters must not block on Library writer");
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0]["title"], "Part 1");
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);

    // (b) First 404 no_submissions + subsequent NegativeCache hit with cached_chapters = Some([Part 1])
    let first_miss = tokio::time::timeout(fast, get_chapters(&f, 1))
        .await
        .expect("404 cloud miss on cached chapters must not acquire Library writer");
    assert_eq!(first_miss[0]["title"], "Part 1");
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    let neg_cached = tokio::time::timeout(fast, get_chapters(&f, 1))
        .await
        .expect("NegativeCache hit GET chapters must not block on Library writer");
    assert_eq!(neg_cached[0]["title"], "Part 1");
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);

    // (c) Active global cooldown (429 DailyQuota) with cached_chapters = Some([Part 1])
    server.respond_with_headers(
        429,
        &[
            ("X-UsageLimit-Reset", "3600"),
            ("X-UsageLimit-Remaining", "0"),
        ],
        r#"{"error":"Usage limit exceeded","code":"usage_limit_exceeded"}"#,
    );
    let trip_cd = tokio::time::timeout(fast, get_chapters(&f, 2))
        .await
        .expect("429 cooldown trip on cached chapters must not acquire Library writer");
    assert_eq!(trip_cd[0]["title"], "Part 1");
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);

    let during_cd = tokio::time::timeout(fast, get_chapters(&f, 2))
        .await
        .expect("cooldown-active GET chapters must not block on Library writer");
    assert_eq!(during_cd[0]["title"], "Part 1");
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);

    let _ = release_tx.send(());
    writer_holder.join().unwrap();
}
