//! Test for force-refreshing chapters and intro/outro markers via API.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

fn app(tmp: &tempfile::TempDir) -> (axum::Router, api::Store) {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let st = state(tmp.path(), fetcher, downloader);
    (router(st), store)
}

#[tokio::test]
async fn force_refresh_item_chapters_keeps_old_result_until_background_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store) = app(&tmp);

    let tv_dir = tmp.path().join("data/library/tv/Frieren");
    std::fs::create_dir_all(&tv_dir).unwrap();

    let strm_file = tv_dir.join("Frieren.S01E01.strm");
    std::fs::write(&strm_file, "https://cdn.example.com/frieren/ep01.mkv\n").unwrap();

    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "葬送的芙莉莲".into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let media = store.ensure_media(media).unwrap();

    let file_id = domain::LedgerId::new();
    let strm_path = strm_file.display().to_string();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(file_id),
            media_id: media.id,
            path: strm_path,
            season: Some(1),
            episode: Some(1),
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            ..Default::default()
        },
    )
    .unwrap();

    // 1. Manually populate cached marker with old/wrong timestamp
    let old_marker = api::store::StoredMediaMarker {
        media_id: media.id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(111_000),
        intro_end_ms: Some(222_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "old_cache".into(),
        locked: false,
        updated_at: 0,
    };
    store.put_media_marker(&old_marker).unwrap();

    // 2. Fetch default library id
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let tv_lib_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();

    // 3. GET /libraries/{id}/items/{item_id}/chapters returns old cached marker
    let get_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{tv_lib_id}/items/{}/chapters", media.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);
    let get_body = json_body(get_res).await;
    let chapters = get_body["data"].as_array().unwrap();
    // 自动为无章节视频构造完整时间轴（[0~111s] 序幕 + [111s~222s] 片头 + [222s~] 正片）
    assert_eq!(chapters.len(), 3);
    let intro = chapters
        .iter()
        .find(|c| c["marker_type"] == "IntroStart")
        .expect("IntroStart marker");
    assert_eq!(intro["start_ms"], 111_000);
    assert_eq!(intro["end_ms"], 222_000);

    // 4. Refresh always re-probes while retaining the currently visible result.
    let refresh_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/libraries/{tv_lib_id}/items/{}/chapters/refresh",
                media.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh_res.status(), StatusCode::OK);
    let refresh_body = json_body(refresh_res).await;
    assert_eq!(
        refresh_body["data"]["chapters"].as_array().unwrap().len(),
        3
    );
    assert_eq!(refresh_body["data"]["fingerprint_refresh_queued"], true);
    assert_eq!(
        refresh_body["data"]["fingerprint_refresh_already_running"],
        false
    );

    let status_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/libraries/{tv_lib_id}/items/{}/probe-status?season=1",
                media.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(status_res.status(), StatusCode::OK);
    let status_body = json_body(status_res).await;
    assert!(status_body["data"]["active"].is_boolean());
    assert_eq!(status_body["data"]["job"]["kind"], "marker_refresh");
    assert_eq!(status_body["data"]["job"]["total"], 1);

    // The previous marker remains available until background probing replaces it.
    let marker = store
        .get_media_marker(media.id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_start_ms, Some(111_000));
    assert_eq!(marker.intro_end_ms, Some(222_000));

    let second_path = tv_dir.join("Frieren.S01E02.strm");
    std::fs::write(&second_path, "https://cdn.example.com/frieren/ep02.mkv\n").unwrap();
    let second_id = domain::LedgerId::new();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(second_id),
            media_id: media.id,
            path: second_path.display().to_string(),
            season: Some(1),
            episode: Some(2),
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(tv_dir.join("Frieren.S01E01-chapter_0.jpg"), b"episode one").unwrap();
    std::fs::write(tv_dir.join("Frieren.S01E02-chapter_0.jpg"), b"episode two").unwrap();
    for (file_id, expected) in [
        (&file_id, b"episode one".as_slice()),
        (&second_id, b"episode two".as_slice()),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/chapters/{}/0", file_id),
                None,
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), expected);
    }
}

async fn multiunit_fixture(
    tmp: &tempfile::TempDir,
) -> (axum::Router, api::Store, domain::MediaId, String) {
    let (app, store) = app(tmp);
    let root = tmp.path().join("data/library/tv/Combined");
    std::fs::create_dir_all(&root).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Tv,
            title: "Combined".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = root.join("S01E01E02.strm");
    std::fs::write(&path, "https://example.invalid/combined.mkv\n").unwrap();
    let file = store::test_support::publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[
            domain::UnitKey::new(1, 1),
            domain::UnitKey::new(1, 2),
            domain::UnitKey::new(2, 1),
        ],
    )
    .unwrap();
    for (season, episode, start) in [(1, 1, 10_000), (1, 2, 120_000), (2, 1, 220_000)] {
        store
            .put_media_marker(&api::store::StoredMediaMarker {
                media_id: media.id,
                season,
                episode,
                intro_start_ms: Some(start),
                intro_end_ms: Some(start + 50_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "review".into(),
                locked: false,
                updated_at: 0,
            })
            .unwrap();
    }
    (app, store, media.id, file.library_id.to_string())
}

#[tokio::test]
async fn chapter_query_uses_explicit_unit_without_first_episode_cache_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _store, media_id, library_id) = multiunit_fixture(&tmp).await;
    for (season, episode, start) in [
        (1, 1, 10_000),
        (1, 2, 120_000),
        (2, 1, 220_000),
        (1, 2, 120_000),
    ] {
        let response = app.clone().oneshot(request("GET", &format!("/api/v1/libraries/{library_id}/items/{media_id}/chapters?season={season}&episode={episode}"), Some("management-secret"), Value::Null)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        let intro = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|chapter| chapter["marker_type"] == "IntroStart")
            .unwrap();
        assert_eq!(intro["start_ms"], start);
    }
}

#[tokio::test]
async fn multiunit_refresh_reports_unsupported_without_clearing_any_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, media_id, library_id) = multiunit_fixture(&tmp).await;
    let response = app.oneshot(request("POST", &format!("/api/v1/libraries/{library_id}/items/{media_id}/chapters/refresh?season=1&episode=2"), Some("management-secret"), Value::Null)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert_eq!(
        body["error"]["code"],
        "chapters.unit_time_mapping_unsupported"
    );
    for (season, episode) in [(1, 1), (1, 2), (2, 1)] {
        assert!(
            store
                .get_media_marker(media_id, Some(season), Some(episode))
                .unwrap()
                .is_some()
        );
    }
}
