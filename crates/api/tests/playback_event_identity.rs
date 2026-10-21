#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn late_old_stop_does_not_close_new_session() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("movie_a.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("movie_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // 1. Start session A
    let session_id_a = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_a.id, 1000)
    };

    // Start A with play_session_id_a
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.id.to_string(),
                "event": "start",
                "position_ms": 100,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 2. Start session B on same device
    let session_id_b = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_b.id, 2000)
    };
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_b.media_id.to_string(),
                "file_id": row_b.id.to_string(),
                "event": "start",
                "position_ms": 200,
                "play_session_id": session_id_b,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Verify active session is B
    let store_arc = fix.state.store();
    let current = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(current.ledger_id.as_deref(), Some(&row_b.id.to_string()[..]));
    assert_eq!(current.play_session_id.as_deref(), Some(session_id_b.as_str()));

    // 3. Late STOP from session A arrives!
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.id.to_string(),
                "event": "stop",
                "position_ms": 150,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Session B must NOT be closed!
    let current_after = store_arc.lock().get_session(user_id, "dev1").unwrap();
    assert!(current_after.is_some(), "Session B must still be active");
    assert_eq!(
        current_after.unwrap().play_session_id.as_deref(),
        Some(session_id_b.as_str())
    );
}

#[tokio::test]
async fn late_old_progress_does_not_rewind_position() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("same_movie.mkv");
    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Session 1 on same file
    let id_1 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.id, 1000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 5000,
            "play_session_id": id_1,
        }),
    ).await;

    // Session 2 on same file (newer generation)
    let id_2 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.id, 2000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 8000,
            "play_session_id": id_2,
        }),
    ).await;

    // Late progress from session 1 with position 2000 arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "progress",
            "position_ms": 2000,
            "play_session_id": id_1,
        }),
    ).await;

    // Active session position must remain 8000, not rewound to 2000
    let store_arc = fix.state.store();
    let cur = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(cur.position_ms, 8000);
}

#[tokio::test]
async fn merged_e02_heartbeat_updates_only_e02() {
    let fix = PresenceFixture::new();
    let tv_lib = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        let tv_root = fix.root.join("tv");
        std::fs::create_dir_all(&tv_root).unwrap();
        guard
            .create_library(
                domain::MediaKind::Tv,
                "TV Shows",
                &[tv_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    };

    let media_id = domain::MediaId::new();
    let show = domain::Media {
        id: media_id,
        kind: domain::MediaKind::Tv,
        title: "Test Show".into(),
        original_title: None,
        aliases: Vec::new(),
        year: Some(2024),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let file_id = domain::LedgerId::new();
    let root_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        let roots = guard.list_library_roots().unwrap();
        roots
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&tv_lib.id))
            .expect("tv root")
            .id
    };
    let file = domain::LibraryFile {
        file_id,
        library_id: tv_lib.id.parse().unwrap(),
        root_id,
        media_id,
        kind: domain::MediaKind::Tv,
        path: std::path::PathBuf::from("Test Show/S01E01-E02.mkv"),
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: Some(10_000_000),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: Some(100),
        release_quality: Some("1080p WEB-DL".into()),
        duration_ms: Some(5_400_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };

    let units = [domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)];
    let scope = store::library_write::WriteScope {
        library_id: tv_lib.id.parse().unwrap(),
        config_revision: 0,
    };

    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.insert_media(&show).unwrap();
        guard
            .publish_files(
                &scope,
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &units,
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Issue session for E02
    let session_id_e02 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .issue_play_session(
                user_id,
                "dev_tv",
                file_id,
                media_id,
                domain::UnitKey::new(1, 2),
                &file.version,
                1000,
            )
            .unwrap()
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev_tv",
            "media_item_id": media_id.to_string(),
            "file_id": file_id.to_string(),
            "season": 1,
            "episode": 2,
            "event": "start",
            "position_ms": 0,
            "play_session_id": session_id_e02,
        }),
    )
    .await;

    // Set initial E01 state (e.g. at 2000ms)
    let session_id_e01 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .issue_play_session(
                user_id,
                "dev_other",
                file_id,
                media_id,
                domain::UnitKey::new(1, 1),
                &file.version,
                1000,
            )
            .unwrap()
    };
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .commit_playback_event(
                &store::playback::BoundPlaybackEvent {
                    user_id,
                    device_id: "dev_other".into(),
                    session_id: session_id_e01,
                    generation: 1,
                    event_sequence: Some(1),
                    item: domain::TypedItemId::Unit {
                        media: media_id,
                        key: domain::UnitKey::new(1, 1),
                    },
                    source_id: domain::PhysicalSourceId {
                        file_id,
                        revision: 1,
                    },
                    source_version: "v1".into(),
                    position_ms: 2000,
                    duration_ms: Some(2_700_000),
                    audio_track: None,
                    subtitle_track: None,
                    paused: false,
                    rate_bps: 0,
                    client: Some("web".into()),
                    event_type: store::playback::PlaybackEventType::Progress,
                },
                1000,
            )
            .unwrap();
    }

    let e01_before = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.unit_state(user_id, media_id, 1, 1).unwrap().unwrap()
    };

    // Send E02 progress heartbeat
    let (status, body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev_tv",
                "media_item_id": media_id.to_string(),
                "file_id": file_id.to_string(),
                "season": 1,
                "episode": 2,
                "event": "progress",
                "position_ms": 5000,
                "play_session_id": session_id_e02,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["ended_by_admin"], false);
    assert_eq!(body["data"]["position_ms"], 5000);

    let e01_after = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.unit_state(user_id, media_id, 1, 1).unwrap().unwrap()
    };
    assert_eq!(e01_after, e01_before);

    let e02_after = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.unit_state(user_id, media_id, 1, 2).unwrap().unwrap()
    };
    assert_eq!(e02_after.position_ms, 5000);
}

#[tokio::test]
async fn issued_session_cannot_write_replacement_source() {
    let fix = PresenceFixture::new();
    let (_media_a, row_a) = fix.insert_media_and_ledger("file_a.mkv");
    let (media_b, row_b) = fix.insert_media_and_ledger("file_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let session_id_a = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_a.id, 1000)
    };

    // Client sends heartbeat for media_b with session_id_a (issued for row_a / media_a)
    // Event should be rejected/ignored and not write to unit or session
    let (status, _body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": media_b.to_string(),
                "file_id": row_b.id.to_string(),
                "event": "progress",
                "position_ms": 10000,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    // Protocol contract: success response (IgnoredStale), but no state was updated
    assert_eq!(status, StatusCode::OK);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let unit_b = guard.unit_state(user_id, media_b, -1, -1).unwrap();
    assert!(unit_b.is_none() || unit_b.unwrap().position_ms != 10000);
}

#[tokio::test]
async fn late_other_episode_does_not_close_active_session() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("ep1.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("ep2.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Start session on B (active)
    let session_id_b = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_b.id, 2000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row_b.media_id.to_string(),
            "file_id": row_b.id.to_string(),
            "event": "start",
            "position_ms": 200,
            "play_session_id": session_id_b,
        }),
    )
    .await;

    // Late stop without ID for A arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row_a.media_id.to_string(),
            "file_id": row_a.id.to_string(),
            "event": "stop",
            "position_ms": 150,
        }),
    )
    .await;

    // Active session B must not be closed!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let active = guard.get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(active.play_session_id.as_deref(), Some(session_id_b.as_str()));
}

#[tokio::test]
async fn duplicate_stop_only_records_one_log() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("single.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let session_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.id, 1000)
    };

    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 1000,
            "play_session_id": session_id,
        }),
    )
    .await;

    // First stop
    let (s1, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.id.to_string(),
                "event": "stop",
                "position_ms": 5000,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(s1, StatusCode::OK);

    // Duplicate stop
    let (s2, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.id.to_string(),
                "event": "stop",
                "position_ms": 5000,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(s2, StatusCode::OK);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let logs = guard
        .list_logs(10, None, None, None, i64::MAX / 2, None)
        .unwrap();
    assert_eq!(logs.len(), 1, "Duplicate stop should only generate 1 log entry");
}
