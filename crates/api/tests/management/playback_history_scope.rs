use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn seed_test_library(app: &axum::Router, name: &str, dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            serde_json::json!({
                "kind": "movie",
                "name": name,
                "root_paths": [dir.to_str().unwrap()],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = json_body(res).await;
    body["data"]["id"].as_str().unwrap().to_string()
}

fn seed_test_ledger(tmp: &tempfile::TempDir, lib_dir: &std::path::Path) -> domain::MediaId {
    let media_id = domain::MediaId::new();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "Movie A".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            bangumi_id: None,
            anilist_id: None,
            tvdb_id: None,
        })
        .unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: lib_dir.join("Movie.A.1999.mkv").to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    media_id
}

#[tokio::test]
async fn empty_library_history_clear_preserves_other_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let lib_a_id = seed_test_library(&app, "Movie Library A", &tmp.path().join("lib_a")).await;
    let _ = lib_a_id;
    let media_id = seed_test_ledger(&tmp, &tmp.path().join("lib_a"));
    let lib_b_id = seed_test_library(&app, "Movie Library B", &tmp.path().join("lib_b")).await;

    // 2. 模拟用户上报该属于 lib_a 的媒体播放进度
    let session =
        super::playback::open_play_session(&app, &media_id.to_string(), "test-device-1").await;
    let progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            serde_json::json!({ "event_sequence": crate::common::next_playback_sequence(),
                "device_id": "test-device-1",
                "media_item_id": media_id.to_string(),
                "play_session_id": session,
                "event": "start",
                "position_ms": 120000,
                "duration_ms": 7200000,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(progress_res.status(), StatusCode::OK);

    // 3. 针对空的 lib_b 发起历史清理：绝不能误删 lib_a 的进度！
    let clear_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=library&library_id={lib_b_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_res.status(), StatusCode::OK);
    let clear_data: Value = json_body(clear_res).await;
    assert_eq!(
        clear_data["data"]["deleted_states"], 0,
        "空库清理应删除 0 条记录，绝不能变成全删"
    );

    // 4. 验证 lib_a 的进度依然存在
    let admin_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let unit = store
        .unit_state(admin_id, media_id, -1, -1)
        .unwrap()
        .expect("lib_a 媒体进度必须保留");
    assert_eq!(unit.position_ms, 120000, "原进度必须被完好保留");
}

#[tokio::test]
async fn invalid_scope_or_id_rejected_without_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 非法 scope
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=bogus",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // scope=item 但缺失或非法 media_item_id
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=item&media_item_id=not-a-uuid",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // scope=library 但 library 不存在
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=library&library_id=nonexistent",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clear_history_hides_inaccessible_library_and_returns_conflict_on_merging_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_member_id, member_token) = create_member(&app, "hist-member").await;
    let hidden_dir = tmp.path().join("hidden-lib");
    std::fs::create_dir_all(&hidden_dir).unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let hidden_lib = store
        .create_library(
            domain::MediaKind::Movie,
            "Hidden",
            &[hidden_dir.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();

    let hidden_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!(
                "/api/v1/playback/history?scope=library&library_id={}",
                hidden_lib.id
            ),
            Some(&member_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(hidden_res.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_error(hidden_res).await["code"], "library.missing");

    let merge_dir_a = tmp.path().join("merge-a");
    let merge_dir_b = tmp.path().join("merge-b");
    let _ = seed_test_library(&app, "Merge Lib A", &merge_dir_a).await;
    let _ = seed_test_library(&app, "Merge Lib B", &merge_dir_b).await;
    let survivor = seed_test_ledger(&tmp, &merge_dir_a);
    let loser = seed_test_ledger(&tmp, &merge_dir_b);
    let admin_id = store.admin_user_id().unwrap();
    store
        .mutate_playback(
            loser,
            &store::playback::PlaybackMutation::marks(admin_id, -1, -1, Some(true), None),
            500,
        )
        .unwrap();
    store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000c1".into(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1_000,
        )
        .unwrap();

    let merge_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={loser}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(merge_res.status(), StatusCode::CONFLICT);
    assert_eq!(json_error(merge_res).await["code"], "playback.protected");
}
