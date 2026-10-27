use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind, UnitKey};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file_units};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn movie_whole_marker_requires_single_unit_file() {
    for (units, expected_marker) in [
        (vec![UnitKey::WHOLE], true),
        (vec![UnitKey::WHOLE, UnitKey::new(2, 1)], false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let store = api::Store::open(tmp.path().join("data")).unwrap();
        let media = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: "Multiunit movie".into(),
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
        let path = store
            .library_root(MediaKind::Movie)
            .unwrap()
            .join("multiunit.mkv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"content").unwrap();
        let file = publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            &units,
        )
        .unwrap();
        let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        store
            .put_media_marker(&store::StoredMediaMarker {
                media_id: media.id,
                season: 1,
                episode: 1,
                intro_start_ms: Some(10_000),
                intro_end_ms: Some(20_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "test".into(),
                locked: false,
                updated_at: 0,
            })
            .unwrap();
        store
            .put_cached_chapters(&file.file_id.to_string(), &[])
            .unwrap();
        let fetcher = Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        });
        let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
        let app_state = state(tmp.path(), fetcher, downloader);
        let chapters = api::marker_resolver::resolve_item_chapters(
            &app_state,
            &row,
            UnitKey::WHOLE,
            &media,
            None,
        )
        .await;
        assert_eq!(
            chapters.iter().any(|chapter| chapter.start_ms == 10_000),
            expected_marker
        );
        let session = store
            .issue_play_session(
                store.admin_user_id().unwrap(),
                "multiunit-movie",
                file.file_id,
                media.id,
                UnitKey::WHOLE,
                &file.version,
                0,
            )
            .unwrap();
        let response = router(app_state)
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/playback/chapters?play_session_id={session}&device_id=multiunit-movie"
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data = json_data(response).await;
        assert_eq!(
            data["chapters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|chapter| chapter["start_ms"] == 10_000),
            expected_marker
        );
    }
}
