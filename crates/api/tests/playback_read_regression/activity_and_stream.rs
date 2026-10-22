use api::router;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{LedgerId, Media, MediaKind};
use serde_json::Value;
use store::Store;
use tower::ServiceExt;

use super::common::{
    add_ledger, get, json_data, make_state, now_secs, seed_library, seed_media, seed_member,
};

#[tokio::test]
async fn activity_media_target_fields_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "activity");
    let (member, token) = seed_member(&store.lock(), "carol");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Watching", Some(2023));
        let file = add_ledger(&guard, media, &library, "Watching");
        let started = now_secs();
        let play_session_id =
            super::support::session::issue_session(&guard, member, "dev-1", file, started);
        guard
            .upsert_session(&api::store::SessionRow {
                device_id: "dev-1".into(),
                user_id: member,
                media_id: media,
                season: Some(-1),
                episode: Some(-1),
                client: Some("test".into()),
                device_name: Some("test".into()),
                client_version: None,
                play_method: "local".into(),
                position_ms: 50,
                start_position_ms: 0,
                duration_ms: Some(100),
                paused: false,
                watched_ms: 50,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 1,
                admin_ended: false,
                started_at: started,
                last_report_at: started,
                last_report_at_ms: started * 1000,
                ledger_id: None,
                play_session_id: Some(play_session_id),
                last_event_sequence: None,
            })
            .unwrap();
        media
    };

    let data = get(&app, "/api/v1/playback/activity", &token).await;
    assert_eq!(data["hidden_session_count"], 0, "{data}");
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{data}");
    let session = &sessions[0];
    assert_eq!(session["member_name"], "carol");
    assert_eq!(session["user_id"], member.to_string());
    assert_eq!(session["device_id"], "dev-1");
    assert_eq!(session["position_ms"], 50);
    assert_eq!(session["progress_percent"], 50);
    let target = &session["media"];
    assert_eq!(target["media_item_id"], media.to_string());
    assert_eq!(target["title"], "Watching");
    assert_eq!(target["kind"], "movie");
    assert_eq!(target["year"], 2023);
    assert_eq!(target["browsable"], true);
    assert_eq!(target["season_number"], Value::Null);
    assert_eq!(target["episode_number"], Value::Null);
    assert_eq!(target["episode_title"], Value::Null);
    let library_id = target["library_id"].as_str().unwrap();
    assert!(
        !library_id.is_empty(),
        "owned single library must be reported"
    );
}

#[tokio::test]
async fn episode_media_target_keeps_numeric_coordinates() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = tmp.path().join("library-episodes");
    std::fs::create_dir_all(&library).unwrap();
    store
        .lock()
        .create_library(
            MediaKind::Tv,
            "episodes",
            &[library.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let (member, token) = seed_member(&store.lock(), "erin");
    let media = {
        let guard = store.lock();
        let media = Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Tv,
            title: "Episodic".into(),
            year: Some(2022),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        guard.insert_media(&media).unwrap();
        let path = library.join("Episodic-S03E07.mkv");
        std::fs::write(&path, b"x").unwrap();
        let file = LedgerId::new();
        store::test_support::publish_fixture_file(
            &guard,
            &store::test_support::FixtureFile {
                file_id: Some(file),
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(3),
                episode: Some(7),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Probe,
                confidence: domain::Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
        let started = now_secs();
        let play_session_id =
            super::support::session::issue_session(&guard, member, "dev-2", file, started);
        guard
            .upsert_session(&api::store::SessionRow {
                device_id: "dev-2".into(),
                user_id: member,
                media_id: media.id,
                season: Some(3),
                episode: Some(7),
                client: Some("test".into()),
                device_name: Some("test".into()),
                client_version: None,
                play_method: "local".into(),
                position_ms: 50,
                start_position_ms: 0,
                duration_ms: Some(100),
                paused: false,
                watched_ms: 50,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 1,
                admin_ended: false,
                started_at: started,
                last_report_at: started,
                last_report_at_ms: started * 1000,
                ledger_id: None,
                play_session_id: Some(play_session_id),
                last_event_sequence: None,
            })
            .unwrap();
        media.id
    };

    let data = get(&app, "/api/v1/playback/activity", &token).await;
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{data}");
    let target = &sessions[0]["media"];
    assert_eq!(target["media_item_id"], media.to_string());
    assert_eq!(target["season_number"], 3);
    assert_eq!(target["episode_number"], 7);
}

#[tokio::test]
async fn media_in_two_libraries_reports_null_library_id() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let first = seed_library(&store.lock(), tmp.path(), "first");
    let second = seed_library(&store.lock(), tmp.path(), "second");
    let (member, token) = seed_member(&store.lock(), "dave");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Shared", Some(2020));
        let file = add_ledger(&guard, media, &first, "Shared-A");
        add_ledger(&guard, media, &second, "Shared-B");
        let started = now_secs();
        let play_session_id =
            super::support::session::issue_session(&guard, member, "dev-2", file, started);
        guard
            .upsert_session(&api::store::SessionRow {
                device_id: "dev-2".into(),
                user_id: member,
                media_id: media,
                season: Some(-1),
                episode: Some(-1),
                client: None,
                device_name: None,
                client_version: None,
                play_method: "local".into(),
                position_ms: 0,
                start_position_ms: 0,
                duration_ms: None,
                paused: false,
                watched_ms: 0,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 1,
                admin_ended: false,
                started_at: started,
                last_report_at: started,
                last_report_at_ms: started * 1000,
                ledger_id: None,
                play_session_id: Some(play_session_id),
                last_event_sequence: None,
            })
            .unwrap();
        media
    };

    let data = get(&app, "/api/v1/playback/activity", &token).await;
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{data}");
    let target = &sessions[0]["media"];
    assert_eq!(target["media_item_id"], media.to_string());
    assert_eq!(
        target["library_id"],
        Value::Null,
        "ambiguous ownership must stay null: {data}"
    );
}

#[tokio::test]
async fn session_stream_serves_valid_video_bytes_with_range_support() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let lib_root = seed_library(&store.lock(), tmp.path(), "stream-test");
    let (_member, token) = seed_member(&store.lock(), "stream-user");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Stream Video", Some(2025));
        let video_path = lib_root.join("sample.mp4");
        std::fs::write(&video_path, b"abcdefghijklmnopqrstuvwxyz0123456789").unwrap();
        store::test_support::publish_fixture_file(
            &guard,
            &store::test_support::FixtureFile {
                file_id: None,
                media_id: media,
                path: video_path.display().to_string(),
                season: None,
                episode: None,
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: domain::QualitySource::Release,
                confidence: domain::Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
        media
    };

    let session_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/playback/sessions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "media_item_id": media.to_string(),
                        "device_id": "web-player",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let session_data = json_data(session_res).await;
    let stream_url = session_data["stream_url"]
        .as_str()
        .expect("must have stream_url");
    assert!(stream_url.starts_with("/api/v1/playback/sessions/"));

    let stream_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(stream_url)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_res.status(), axum::http::StatusCode::OK);
    let bytes = to_bytes(stream_res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"abcdefghijklmnopqrstuvwxyz0123456789");

    let range_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(stream_url)
                .header("authorization", format!("Bearer {token}"))
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(range_res.status(), axum::http::StatusCode::PARTIAL_CONTENT);
    let range_bytes = to_bytes(range_res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&range_bytes[..], b"abcd");
}

#[tokio::test]
async fn web_movie_playback_omits_coords_and_explicit_zero_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let store = state.store();
    let app = router(state.clone());
    let lib_root = seed_library(&store.lock(), tmp.path(), "coord-test");
    let (_member, token) = seed_member(&store.lock(), "coord-user");

    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Coord Movie", Some(2025));
        let video_path = lib_root.join("movie.mp4");
        std::fs::write(&video_path, b"movie-content-1234").unwrap();
        store::test_support::publish_fixture_file(
            &guard,
            &store::test_support::FixtureFile {
                file_id: None,
                media_id: media,
                path: video_path.display().to_string(),
                season: None,
                episode: None,
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: domain::QualitySource::Release,
                confidence: domain::Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
        media
    };

    let req_omit = Request::builder()
        .method("POST")
        .uri("/api/v1/playback/sessions")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "media_item_id": media.to_string(),
                "device_id": "web-player",
            }))
            .unwrap(),
        ))
        .unwrap();
    let res_omit = app.clone().oneshot(req_omit).await.unwrap();
    assert_eq!(res_omit.status(), axum::http::StatusCode::OK);

    let req_zeros = Request::builder()
        .method("POST")
        .uri("/api/v1/playback/sessions")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "media_item_id": media.to_string(),
                "season_number": 0,
                "episode_number": 0,
                "device_id": "web-player",
            }))
            .unwrap(),
        ))
        .unwrap();
    let res_zeros = app.clone().oneshot(req_zeros).await.unwrap();
    assert_eq!(res_zeros.status(), axum::http::StatusCode::BAD_REQUEST);
}
