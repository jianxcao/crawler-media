#[path = "management/common.rs"]
mod common;
#[path = "support/theintrodb.rs"]
mod fixture;

use domain::{Media, MediaId, MediaKind, UnitKey};
use fixture::Server;
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};

const RESPONSE: &str = r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"group_duration_ms":2700000,"intro":[{"start_ms":null,"end_ms":70500,"confidence":0.87,"submission_count":12}],"credits":[{"start_ms":2600000,"end_ms":null,"confidence":0.9,"submission_count":12}]}"#;

struct NoMediaReads;
impl api::marker_resolver::ChapterProbe for NoMediaReads {
    fn probe(&self, _: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        panic!("cloud marker lookup must not read the STRM Media source")
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Arc<api::Store>,
    state: api::ApiState,
    media: Media,
    row: domain::FileCoverage,
}

fn fixture(duration: Option<i64>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(api::Store::open(tmp.path().join("data")).unwrap());
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Cloud marker".into(),
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
    store
        .create_library(
            MediaKind::Tv,
            "Cloud marker",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("S01E02.strm");
    std::fs::write(&path, "https://fixture.invalid/video.mkv").unwrap();
    let file = store::test_support::publish_fixture_file_units(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 2)],
    )
    .unwrap();
    store
        .set_library_intro_settings(&file.library_id.to_string(), true, false)
        .unwrap();
    store
        .publish_probe_result(
            file.file_id,
            &store::library_write::probe::ProbePublication {
                expected: &file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: duration,
                details: None,
            },
            0,
        )
        .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_chapter_probe(Arc::new(NoMediaReads));
    Fixture {
        _tmp: tmp,
        store,
        state,
        media,
        row,
    }
}

async fn resolve(f: &Fixture, server: &Server) -> Vec<marker::ChapterMarker> {
    api::marker_resolver::resolve_item_chapters(
        &f.state,
        &f.row,
        UnitKey::new(1, 2),
        &f.media,
        Some(server.client(true)),
    )
    .await
}

#[tokio::test]
async fn cloud_marker_is_published_and_overlaid_with_fingerprinting_disabled_and_empty_chapter_cache()
 {
    let f = fixture(Some(2700000));
    f.store
        .put_cached_chapters(&f.row.file.file_id.to_string(), &[])
        .unwrap();
    let server = Server::new(200, RESPONSE, true);
    let chapters = resolve(&f, &server).await;
    let stored = f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .expect("cloud timestamps must reach the file marker consumer");
    assert_eq!(stored.source, "theintrodb");
    assert_eq!(stored.version, f.row.file.version);
    assert_eq!(
        (stored.intro_start_ms, stored.intro_end_ms),
        (Some(0), Some(70500))
    );
    assert_eq!(
        (stored.outro_start_ms, stored.outro_end_ms),
        (Some(2600000), Some(2700000))
    );
    assert!(chapters.iter().any(|chapter| chapter.marker_type
        == Some(marker::MarkerType::IntroStart)
        && chapter.end_ms == 70500));
}

#[tokio::test]
async fn cloud_marker_keeps_unknown_duration_and_fallback_release_as_candidates() {
    for duration in [None, Some(2000000)] {
        let f = fixture(duration);
        let server = Server::for_duration(200, RESPONSE, true, duration);
        assert!(resolve(&f, &server).await.is_empty());
        assert!(
            f.store
                .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn a_manual_marker_published_during_cloud_http_keeps_its_lock_and_ranges() {
    let f = fixture(Some(2700000));
    let store = f.store.clone();
    let row = f.row.clone();
    let server = Server::with_action(200, RESPONSE, true, move || {
        let marker = store::StoredFileMarker {
            file_id: row.file.file_id,
            version: row.file.version.clone(),
            unit: UnitKey::new(1, 2),
            intro_start_ms: Some(20000),
            intro_end_ms: Some(80000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "manual".into(),
            locked: true,
            updated_at: 0,
        };
        let members = store::observe_sources(&store.source_snapshots(&[row.file.file_id]).unwrap());
        store.put_file_marker_checked(&marker, &members).unwrap();
    });
    let chapters = resolve(&f, &server).await;
    let marker = f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .unwrap();
    assert!(marker.locked);
    assert_eq!(marker.source, "manual");
    assert_eq!(marker.intro_start_ms, Some(20000));
    assert!(chapters.iter().any(|chapter| chapter.marker_type
        == Some(marker::MarkerType::IntroStart)
        && chapter.start_ms == 20000));
}

#[tokio::test]
async fn source_changed_during_cloud_http_rejects_the_old_file_result() {
    let f = fixture(Some(2700000));
    let path = f.row.file.path.clone();
    let server = Server::with_action(200, RESPONSE, true, move || {
        std::fs::write(&path, "https://fixture.invalid/changed-cut.mkv").unwrap();
    });
    assert!(resolve(&f, &server).await.is_empty());
    assert!(
        f.store
            .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn duration_corrected_during_cloud_http_rejects_the_old_release_selection() {
    let f = fixture(Some(2700000));
    let store = f.store.clone();
    let row = f.row.clone();
    let server = Server::with_action(200, RESPONSE, true, move || {
        store
            .publish_probe_result(
                row.file.file_id,
                &store::library_write::probe::ProbePublication {
                    expected: &row.file.version,
                    tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                    resolution: None,
                    codec: None,
                    hdr: store::library_write::probe::Observed::NotObserved,
                    duration_ms: Some(2000000),
                    details: None,
                },
                0,
            )
            .unwrap();
    });
    assert!(resolve(&f, &server).await.is_empty());
    assert!(
        f.store
            .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store
            .file_coverage_by_id(f.row.file.file_id)
            .unwrap()
            .unwrap()
            .file
            .duration_ms,
        Some(2000000)
    );
}

#[tokio::test]
async fn missing_or_invalid_cloud_data_never_becomes_a_file_marker() {
    for (status, body) in [
        (
            404,
            r#"{"error":"No accepted submissions","code":"no_submissions"}"#.to_string(),
        ),
        (200, RESPONSE.replace("\"group_duration_ms\":2700000,", "")),
        (
            200,
            RESPONSE.replace("\"end_ms\":70500", "\"end_ms\":2800000"),
        ),
        (
            200,
            RESPONSE.replace("\"start_ms\":2600000", "\"start_ms\":70000"),
        ),
        (200, RESPONSE.replace("\"episode\":2", "\"episode\":3")),
    ] {
        let f = fixture(Some(2700000));
        let server = Server::new(status, &body, true);
        assert!(resolve(&f, &server).await.is_empty());
        assert!(
            f.store
                .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn physical_intro_published_during_cloud_lookup_wins_and_its_cache_survives() {
    let f = fixture(Some(2700000));
    let store = f.store.clone();
    let row = f.row.clone();
    f.store
        .put_cached_chapters(&row.file.file_id.to_string(), &[])
        .unwrap();
    let physical = vec![marker::ChapterMarker {
        start_ms: 25000,
        end_ms: 85000,
        title: Some("Physical Intro".into()),
        synthetic: false,
        marker_type: Some(marker::MarkerType::IntroStart),
    }];
    let published = physical.clone();
    let server = Server::with_action(200, RESPONSE, true, move || {
        store
            .put_cached_chapters(&row.file.file_id.to_string(), &published)
            .unwrap();
    });
    let chapters = resolve(&f, &server).await;
    assert_eq!(
        f.store
            .get_cached_chapters(&f.row.file.file_id.to_string())
            .unwrap(),
        Some(physical)
    );
    assert!(chapters.iter().any(|chapter| chapter.marker_type
        == Some(marker::MarkerType::IntroStart)
        && chapter.start_ms == 25000));
    let marker = f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .unwrap();
    assert_eq!(marker.source, "chapter");
    assert_eq!(marker.intro_start_ms, Some(25000));
}

struct Clock(std::sync::atomic::AtomicI64);
impl api::playback_events::PlaybackClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn confirmed_no_data_is_reused_until_expiry_then_new_cloud_submissions_are_visible() {
    let mut f = fixture(Some(2700000));
    let clock = Arc::new(Clock(std::sync::atomic::AtomicI64::new(1000)));
    f.state = f.state.with_playback_clock(clock.clone());
    let server = Server::new(
        404,
        r#"{"error":"No accepted submissions","code":"no_submissions"}"#,
        true,
    );
    assert!(resolve(&f, &server).await.is_empty());
    server.respond(200, RESPONSE);
    assert!(
        resolve(&f, &server).await.is_empty(),
        "confirmed empty cloud result should remain stable within its TTL"
    );
    clock.0.store(301001, std::sync::atomic::Ordering::SeqCst);
    assert!(
        !resolve(&f, &server).await.is_empty(),
        "new submissions must become visible after the negative cache expires"
    );
}

#[tokio::test]
async fn explicit_refresh_bypasses_the_cloud_negative_cache() {
    let f = fixture(Some(2700000));
    let server = Server::new(
        404,
        r#"{"error":"No accepted submissions","code":"no_submissions"}"#,
        true,
    );
    assert!(resolve(&f, &server).await.is_empty());
    server.respond(200, RESPONSE);
    let chapters = api::marker_resolver::force_refresh_item_chapters(
        &f.state,
        &f.row,
        UnitKey::new(1, 2),
        &f.media,
        Some(server.client(true)),
    )
    .await
    .unwrap();
    assert!(!chapters.is_empty());
}

#[tokio::test]
async fn changing_introdb_settings_expires_the_previous_negative_result() {
    use tower::ServiceExt;
    let mut f = fixture(Some(2700000));
    let empty = Server::new(
        404,
        r#"{"error":"No accepted submissions","code":"no_submissions"}"#,
        false,
    );
    enable_cloud(&mut f, &empty);
    let app = api::router(f.state.clone());
    let url = format!(
        "/api/v1/libraries/{}/items/{}/chapters?file_id={}&season=1&episode=2",
        f.row.file.library_id, f.media.id, f.row.file.file_id
    );
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            &url,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        common::json_data(response)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );

    f.store
        .save_raw_scrape_settings(
            r#"{"theintrodb_enabled":true,"theintrodb_api_key":"fixture-key"}"#,
        )
        .unwrap();
    let data = Server::new(200, RESPONSE, true);
    f.state = f
        .state
        .clone()
        .with_theintrodb_factory(Arc::new(LocalClient(data.url.clone())));
    let app = api::router(f.state.clone());
    let response = app
        .oneshot(common::request(
            "GET",
            &url,
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        common::json_data(response)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|chapter| chapter["marker_type"] == "IntroStart")
    );
}

#[tokio::test]
async fn changed_file_duration_atomically_invalidates_cloud_markers() {
    let f = fixture(Some(2700000));
    let server = Server::new(200, RESPONSE, true);
    let chapters = resolve(&f, &server).await;
    assert!(!chapters.is_empty());
    assert_eq!(
        f.store
            .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .unwrap()
            .source,
        "theintrodb"
    );

    f.store
        .publish_probe_result(
            f.row.file.file_id,
            &store::library_write::probe::ProbePublication {
                expected: &f.row.file.version,
                tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
                resolution: None,
                codec: None,
                hdr: store::library_write::probe::Observed::NotObserved,
                duration_ms: Some(2500000),
                details: None,
            },
            0,
        )
        .unwrap();
    assert!(
        f.store
            .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn reidentified_file_during_cloud_lookup_rejects_the_old_response() {
    let f = fixture(Some(2700000));
    let store = f.store.clone();
    let original = f.media.clone();
    let row = f.row.clone();
    let server = Server::with_action(200, RESPONSE, true, move || {
        let target = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Tv,
                title: "Reidentified work".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: Some("456".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        store
            .reassign_files(
                &store::library_write::ReassignFiles {
                    files: vec![(row.file.file_id, row.file.version.clone())],
                    from: original.id,
                    to: target.id,
                    expected_media_revision: store.media_revision(original.id).unwrap().unwrap(),
                },
                0,
            )
            .unwrap();
    });
    assert!(resolve(&f, &server).await.is_empty());
    assert!(
        f.store
            .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
            .unwrap()
            .is_none()
    );
}

struct LocalClient(String);
impl api::theintrodb_lookup::ClientFactory for LocalClient {
    fn create(&self, key: Option<String>) -> api::theintrodb::TheIntroDbClient {
        api::theintrodb::TheIntroDbClient::new(key).with_base_url(&self.0)
    }
}

fn enable_cloud(f: &mut Fixture, server: &Server) {
    f.store
        .save_raw_scrape_settings(r#"{"theintrodb_enabled":true}"#)
        .unwrap();
    f.state = f
        .state
        .clone()
        .with_theintrodb_factory(Arc::new(LocalClient(server.url.clone())));
}

#[tokio::test]
async fn chapters_http_uses_the_enabled_anonymous_cloud_configuration() {
    use tower::ServiceExt;
    let mut f = fixture(Some(2700000));
    let server = Server::new(200, RESPONSE, false);
    enable_cloud(&mut f, &server);
    let response = api::router(f.state.clone())
        .oneshot(common::request(
            "GET",
            &format!(
                "/api/v1/libraries/{}/items/{}/chapters?file_id={}&season=1&episode=2",
                f.row.file.library_id, f.media.id, f.row.file.file_id
            ),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let json = common::json_data(response).await;
    assert!(
        json.as_array()
            .unwrap()
            .iter()
            .any(|chapter| chapter["start_ms"] == 0 && chapter["marker_type"] == "IntroStart")
    );
}

#[tokio::test]
async fn direct_playback_session_schedules_cloud_lookup_with_old_empty_chapter_cache() {
    use tower::ServiceExt;
    let mut f = fixture(Some(2700000));
    let server = Server::new(200, RESPONSE, false);
    enable_cloud(&mut f, &server);
    f.store
        .put_cached_chapters(&f.row.file.file_id.to_string(), &[])
        .unwrap();
    let response = api::router(f.state.clone()).oneshot(common::request("POST", "/api/v1/playback/sessions", Some("management-secret"), serde_json::json!({"media_item_id":f.media.id,"file_id":f.row.file.file_id,"season_number":1,"episode_number":2}))).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let json = common::json_data(response).await;
    assert_eq!(json["chapters_pending"], true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .is_none()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "direct Playback must adopt cloud markers without first opening the chapters API"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn jellyfin_playback_info_schedules_cloud_markers_for_direct_playback() {
    use tower::ServiceExt;
    let mut f = fixture(Some(2700000));
    let server = Server::new(200, RESPONSE, false);
    enable_cloud(&mut f, &server);
    f.store
        .put_cached_chapters(&f.row.file.file_id.to_string(), &[])
        .unwrap();
    let app = api::router(f.state.clone());
    let item = domain::TypedItemId::Unit {
        media: f.media.id,
        key: UnitKey::new(1, 2),
    }
    .to_wire_string();
    let response = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/Items/{item}/PlaybackInfo"),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .is_none()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Jellyfin PlaybackInfo must schedule cloud lookup before the chapters API is opened"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let response = app
        .oneshot(common::request(
            "POST",
            &format!("/Items/{item}/PlaybackInfo"),
            Some("management-secret"),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    let json = common::json_body(response).await;
    assert!(
        json["Chapters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|chapter| chapter["MarkerType"] == "IntroStart"
                && chapter["StartPositionTicks"] == 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_playback_requests_share_one_inflight_cloud_lookup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;
    let mut f = fixture(Some(2700000));
    let release = Arc::new(AtomicBool::new(false));
    let released = release.clone();
    let server = Server::with_action(200, RESPONSE, false, move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !released.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    enable_cloud(&mut f, &server);
    f.store
        .put_cached_chapters(&f.row.file.file_id.to_string(), &[])
        .unwrap();
    let app = api::router(f.state.clone());
    let item = domain::TypedItemId::Unit {
        media: f.media.id,
        key: UnitKey::new(1, 2),
    }
    .to_wire_string();
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let app = app.clone();
        let url = format!("/Items/{item}/PlaybackInfo");
        calls.spawn(async move {
            app.oneshot(common::request(
                "POST",
                &url,
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap()
            .status()
        });
    }
    while let Some(status) = calls.join_next().await {
        assert_eq!(status.unwrap(), axum::http::StatusCode::OK);
    }
    release.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while f
        .store
        .get_file_marker(f.row.file.file_id, UnitKey::new(1, 2))
        .unwrap()
        .is_none()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        1,
        "one file/version must have at most one cloud lookup in flight"
    );
}
