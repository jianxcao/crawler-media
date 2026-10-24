#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use serde_json::json;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

fn fixture(root: &std::path::Path) -> (api::ApiState, axum::Router, MediaId, String) {
    let state = common::state(
        root,
        Arc::new(common::Fixtures {
            requests: Default::default(),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    );
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Canonical movie".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let library_id = {
        let handle = state.store();
        let store = handle.lock();
        store.insert_media(&media).unwrap();
        let path = root.join("data/library/movies/canonical.mkv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"movie").unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        file.library_id.to_string()
    };
    let app = api::router(state.clone());
    (state, app, media.id, library_id)
}

#[tokio::test]
async fn movie_decision_rejects_zero_coordinate_alias() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, media, _) = fixture(temp.path());
    let response = app
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/decide",
            Some("management-secret"),
            json!({"media_item_id": media.to_string(), "season_number": 0, "episode_number": 0}),
        ))
        .await
        .unwrap();
    assert!(!response.status().is_success());
}

#[tokio::test]
async fn playback_member_filters_reject_numeric_user_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, _, _) = fixture(temp.path());
    for route in ["history", "stats/watch"] {
        let response = app
            .clone()
            .oneshot(common::request(
                "GET",
                &format!("/api/v1/playback/{route}?member_id=0"),
                Some("management-secret"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{route}");
    }
}

#[tokio::test]
async fn library_facets_do_not_emit_obsolete_years_alias() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, _, library) = fixture(temp.path());
    let response = app
        .oneshot(common::request(
            "GET",
            &format!("/api/v1/libraries/{library}/facets"),
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    let data = common::json_data(response).await;
    assert!(data.get("years").is_none());
    assert!(data["decades"].is_array());
}

#[tokio::test]
async fn movie_decision_accepts_canonical_whole_request() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, media, _) = fixture(temp.path());
    let response = app
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/decide",
            Some("management-secret"),
            json!({"media_item_id": media.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = common::json_data(response).await;
    assert_eq!(data["outcome"], "plan");
}

#[tokio::test]
async fn tv_zero_season_zero_episode_remains_playable() {
    let temp = tempfile::tempdir().unwrap();
    let (state, app, _, _) = fixture(temp.path());
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Special zero".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    {
        let handle = state.store();
        let store = handle.lock();
        store.insert_media(&media).unwrap();
        let root = temp.path().join("tv-zero");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Zero specials",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let path = root.join("special.S00E00.mkv");
        std::fs::write(&path, b"episode").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(0),
                episode: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let response = app
        .oneshot(common::request(
            "POST",
            "/api/v1/playback/decide",
            Some("management-secret"),
            json!({"media_item_id": media.id.to_string(), "season_number": 0, "episode_number": 0}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = common::json_data(response).await;
    assert_eq!(data["outcome"], "plan");
}

#[tokio::test]
async fn playback_member_filters_accept_canonical_user_uuid() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, _, _) = fixture(temp.path());
    for route in ["history", "stats/watch"] {
        let response = app
            .clone()
            .oneshot(common::request(
                "GET",
                &format!("/api/v1/playback/{route}?member_id=00000000-0000-0000-0000-000000000001"),
                Some("management-secret"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
    }
}

#[tokio::test]
async fn marks_reject_overflow_unknown_and_duplicate_coordinates_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let (_state, app, media, _) = fixture(temp.path());
    for extra in [
        json!({"season_number": 2147483648_u64}),
        json!({"season_number": 4294967295_u64}),
        json!({"season_number": 4294967296_u64}),
        json!({"season_number": -1}),
        json!({"season_number": "1"}),
        json!({"season_number": 1.5}),
        json!({"seasonNumber": 1}),
        json!({"device_id": "unused"}),
        json!({"season": 1}),
    ] {
        let mut body = json!({"media_item_id": media.to_string(), "favorite": true});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = app
            .clone()
            .oneshot(common::request(
                "POST",
                "/api/v1/playback/marks",
                Some("management-secret"),
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    for extra in [
        "season=1",
        "seasonNumber=1",
        "season_number=4294967295",
        "season_number=1&season_number=2",
        "episode_number=1",
    ] {
        let uri = format!("/api/v1/playback/marks?media_item_id={media}&{extra}");
        let response = app
            .clone()
            .oneshot(common::request(
                "GET",
                &uri,
                Some("management-secret"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    let conn = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
    let units: i64 = conn
        .query_row("SELECT count(*) FROM playback_units", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        units, 0,
        "rejected marks must not create whole or clamped unit facts"
    );
}
