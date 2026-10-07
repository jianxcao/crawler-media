//! Playback activity: progress heartbeats → sessions/logs/units, activity
//! snapshot, history, stats, marks, up-next, favorites, devices.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

/// Seed one movie file in a movie library root and scan it, returning the
/// media id via the ledger.
async fn seed_movie(app: &axum::Router, tmp: &tempfile::TempDir) -> String {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    std::fs::write(movie.join("The.Matrix.1999.2160p.mkv"), b"matrix").unwrap();
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
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scan = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan.status(), StatusCode::OK);
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    ledger["data"][0]["media_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn playback_item_uses_ledger_id_for_public_poster_url() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let row = &ledger["data"][0];
    let ledger_id = row["id"].as_str().unwrap();
    let parent = std::path::Path::new(row["path"].as_str().unwrap())
        .parent()
        .unwrap();
    std::fs::write(parent.join("poster.jpg"), b"poster").unwrap();

    let item = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/items/{media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let expected = format!("/posters/{}", ledger_id);
    assert_eq!(item["data"]["poster_url"], expected);

    let poster = app
        .oneshot(request(
            "GET",
            &format!("/api/v1{expected}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        poster.status(),
        StatusCode::OK,
        "visible artwork is authenticated"
    );
}

async fn post_async(app: &axum::Router, uri: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(request("POST", uri, Some("management-secret"), body))
        .await
        .unwrap()
}

#[tokio::test]
async fn progress_lifecycle_writes_session_log_and_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;

    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    let body = json_body(start).await;
    assert_eq!(body["data"]["played"], false);
    assert_eq!(body["data"]["position_ms"], 0);

    let beat = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-1", "position_ms": 120_000, "paused": false }),
    )
    .await;
    assert_eq!(beat.status(), StatusCode::OK);
    let body = json_body(beat).await;
    assert_eq!(body["data"]["position_ms"], 120_000);

    // Active session shows in activity.
    let activity = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/activity",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let sessions = activity["data"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["device_id"], "web-1");
    assert_eq!(
        sessions[0]["revocable"], false,
        "web player has no credentials"
    );
    assert_eq!(sessions[0]["media"]["title"], "The Matrix");

    // Stop closes the session into a play log.
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "position_ms": 300_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

    let activity = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/activity",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(activity["data"]["sessions"].as_array().unwrap().is_empty());

    let history = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/history",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let entries = history["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["media"]["title"], "The Matrix");
    assert_eq!(entries[0]["watched_ms"], 120_000 + 180_000);
    assert_eq!(entries[0]["completed"], false);

    // Resume reads the unit.
    let resume = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/resume?media_item_id={media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(resume["data"]["position_ms"], 300_000);
}

#[tokio::test]
async fn marks_cascade_and_favorites_list() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;

    let marked = post_async(
        &app,
        "/api/v1/playback/marks",
        json!({ "media_item_id": media_id, "played": true, "favorite": true }),
    )
    .await;
    assert_eq!(marked.status(), StatusCode::OK);

    let marks = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/marks?media_item_id={media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(marks["data"]["played"], true);
    assert_eq!(marks["data"]["is_favorite"], true);

    // If an episode is favorited, query for whole show must also reflect is_favorite = true
    let tv_id = uuid::Uuid::new_v4().to_string();
    let _ = post_async(
        &app,
        "/api/v1/playback/marks",
        json!({
            "media_item_id": tv_id,
            "season_number": 1,
            "episode_number": 1,
            "favorite": true
        }),
    )
    .await;

    let tv_whole_marks = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/marks?media_item_id={tv_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(tv_whole_marks["data"]["is_favorite"], true);

    let favorites = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/favorites",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(favorites["data"]["total"], 1);
    assert_eq!(favorites["data"]["items"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn up_next_lists_next_episode_for_tv_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    std::fs::write(tv.join("The.Expanse.S01E01.1080p.mkv"), b"expanse").unwrap();
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
    let tv_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scan = post_async(
        &app,
        &format!("/api/v1/libraries/{tv_id}/scan"),
        Value::Null,
    )
    .await;
    assert_eq!(scan.status(), StatusCode::OK);
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap().to_string();

    let subscribe = create_subscribe(&app, "search").await;
    let _ = subscribe;
    let up_next = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    // No subscription matching this media → empty list (valid shape).
    assert!(up_next["data"]["items"].is_array());

    // 当用户播放了该本地扫库剧集（无订阅）后，即使无订阅也必须出现在 up-next
    let progress_res = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({
            "media_item_id": media_id,
            "season": 1,
            "episode": 1,
            "position_ms": 120_000,
            "duration_ms": 2_400_000,
            "device_id": "test-device"
        }),
    )
    .await;
    assert_eq!(progress_res.status(), StatusCode::OK);

    let up_next2 = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let items = up_next2["data"]["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "本地扫库且无订阅的剧集在播放后成功展示在接下来看"
    );
    assert_eq!(items[0]["media_item_id"], media_id);
    assert_eq!(items[0]["season_number"], 1);
    assert_eq!(items[0]["episode_number"], 1);
    assert_eq!(items[0]["position_ms"], 120_000);
}

#[tokio::test]
async fn end_session_and_revoke_device() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;
    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "tv-1" }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);

    let ended = post_async(
        &app,
        "/api/v1/playback/activity/sessions/tv-1/end",
        Value::Null,
    )
    .await;
    assert_eq!(ended.status(), StatusCode::OK);

    let activity = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/activity",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(activity["data"]["sessions"].as_array().unwrap().is_empty());

    // Revoked device: progress is rejected.
    let start2 = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "blocked-1" }),
    )
    .await;
    assert_eq!(start2.status(), StatusCode::OK);
    let revoke = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/devices/blocked-1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);
    let blocked = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "blocked-1", "position_ms": 10 }),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn stats_aggregate_and_policy_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;
    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "position_ms": 60_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

    let conn = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO playback_logs (id, user_id, media_id, started_at, ended_at, watched_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            "00000000-0000-0000-0000-000000000001",
            media_id,
            now - 8 * 86_400,
            now - 8 * 86_400 + 60,
            30_000
        ],
    )
    .unwrap();

    let stats = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/stats/watch?days=7",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(stats["data"]["current"]["plays"], 1);
    assert!(stats["data"]["current"]["watched_ms"].as_i64().unwrap() >= 60_000);
    assert_eq!(stats["data"]["by_day"].as_array().unwrap().len(), 7);
    assert_eq!(stats["data"]["by_hour"].as_array().unwrap().len(), 7);
    assert_eq!(stats["data"]["by_hour"][0].as_array().unwrap().len(), 24);
    assert!(
        stats["data"]["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["members"] == 1)
    );
    assert_eq!(
        stats["data"]["by_member"][0]["member_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(
        stats["data"]["top_titles"][0]["watched_ms"]
            .as_i64()
            .unwrap()
            >= 60_000
    );
    assert_eq!(stats["data"]["top_titles"][0]["members"], 1);
    assert_eq!(stats["data"]["previous"]["active_members"], 1);
    assert_eq!(
        stats["data"]["previous_by_day"].as_array().unwrap().len(),
        7
    );
    assert!(
        stats["data"]["previous_by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["plays"] == 1)
    );

    let policy = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/playback/policy",
            Some("management-secret"),
            json!({ "trickplay_enabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(policy.status(), StatusCode::OK);
    let policy_get = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/policy",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(policy_get["data"]["trickplay_enabled"], true);
    assert_eq!(policy_get["data"]["hardware_available"], false);
}

#[tokio::test]
async fn playback_decide_and_subtitle_delivery_returns_tracks_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_file = tmp.path().join("data/library/movies/TestFilm.mkv");
    let srt_file = tmp.path().join("data/library/movies/TestFilm.zh.srt");
    std::fs::create_dir_all(movie_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, b"video-data").unwrap();
    std::fs::write(
        &srt_file,
        b"1\n00:00:01,000 --> 00:00:02,000\nHello Subtitle",
    )
    .unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "TestFilm".into(),
            year: Some(2025),
            original_title: None,
            tmdb_id: Some("77777".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: movie_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();

    // 写入该文件的探测轨道数据（包含音轨与外部字幕）
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(1),
                    codec: Some("aac".into()),
                    profile: None,
                    language: Some("chi".into()),
                    title: Some("Chinese Audio".into()),
                    channels: Some(2),
                    bit_rate: None,
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(2),
                    codec: Some("subrip".into()),
                    profile: None,
                    language: Some("chi".into()),
                    title: Some("Chinese Subtitle".into()),
                    bit_rate: None,
                    is_default: true,
                    forced: false,
                    is_external: true,
                    path: Some(srt_file.display().to_string()),
                }],
            },
        )
        .unwrap();
    drop(store);

    // 1. 调用 POST /api/v1/playback/decide
    let decide_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/decide",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(decide_res.status(), StatusCode::OK);
    let decision = json_data(decide_res).await;

    // 验证 audio_tracks 和 subtitles 不为空 (G07)
    let audio_tracks = decision["audio_tracks"].as_array().unwrap();
    let subtitles = decision["subtitles"].as_array().unwrap();
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0]["language"], "chi");
    assert_eq!(subtitles.len(), 1);
    assert_eq!(subtitles[0]["language"], "chi");
    let delivery_url = subtitles[0]["delivery_url"].as_str().unwrap();
    assert!(
        delivery_url.contains(&row.id.to_string()),
        "字幕对象必须包含正确的交付 URL"
    );

    // 2. 调用交付 URL 获取字幕文件内容
    let sub_res = app
        .clone()
        .oneshot(request(
            "GET",
            delivery_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(sub_res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(sub_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Hello Subtitle"),
        "必须成功返回字幕文件内容 (G07)"
    );

    // 3. 验证内封字幕轨交付与缓存命中 (无 external 路径时走内封提取或缓存)
    let cache_dir = std::env::temp_dir().join("crawler-media-subtitles").join(row.id.to_string());
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("sub_3.vtt"), b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nEmbedded Subtitle Cached").unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: vec![],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(3),
                    codec: Some("subrip".into()),
                    profile: None,
                    language: Some("eng".into()),
                    title: Some("English Subtitle".into()),
                    bit_rate: None,
                    is_default: false,
                    forced: false,
                    is_external: false,
                    path: None,
                }],
            },
        )
        .unwrap();
    drop(store);

    let embedded_sub_url = format!("/api/v1/playback/subtitles/{}/3?format=vtt", row.id);
    let embedded_res = app
        .clone()
        .oneshot(request(
            "GET",
            &embedded_sub_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(embedded_res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(embedded_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Embedded Subtitle Cached"),
        "必须成功交付内封字幕 (G07)"
    );
}
