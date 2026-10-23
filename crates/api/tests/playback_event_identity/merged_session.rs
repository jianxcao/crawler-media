use super::*;

#[tokio::test]
async fn merged_e02_heartbeat_updates_only_e02() {
    let fix = PresenceFixture::new();
    let merged = merged_e02_fixture(&fix);
    let session_id_e02 = merged_e02_open_episode_sessions(&fix, &merged).await;
    merged_e02_progress_touches_only_e02(&fix, &merged, &session_id_e02).await;
}

/// 夹具:TV 库 + 一份已发布文件(ledger 单元 1x1 / 1x2 同源),外加 admin 用户。
struct MergedE02Fixture {
    media_id: domain::MediaId,
    file_id: domain::LedgerId,
    user_id: domain::UserId,
    file: domain::LibraryFile,
}

fn merged_e02_fixture(fix: &PresenceFixture) -> MergedE02Fixture {
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
    MergedE02Fixture {
        media_id,
        file_id,
        user_id,
        file,
    }
}

/// 行为步骤:`dev_tv` 的 E02 会话 issue + start,`dev_other` 的 E01 会话 issue + 一条 2000ms 的
/// Progress 事实(基线,后面要证明它逐字不变)。
async fn merged_e02_open_episode_sessions(
    fix: &PresenceFixture,
    merged: &MergedE02Fixture,
) -> String {
    let user_id = merged.user_id;
    let media_id = merged.media_id;
    let file_id = merged.file_id;
    let file = &merged.file;
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
            "season_number": 1,
            "episode_number": 2,
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
                    device_name: None,
                    client_version: None,
                    play_method: None,
                    event_type: store::playback::PlaybackEventType::Progress,
                },
                1000,
            )
            .unwrap();
    }
    session_id_e02
}

/// 行为步骤:E02 心跳前进到 5000ms;E01 事实不得被这次心跳改动。
async fn merged_e02_progress_touches_only_e02(
    fix: &PresenceFixture,
    merged: &MergedE02Fixture,
    session_id_e02: &str,
) {
    let user_id = merged.user_id;
    let media_id = merged.media_id;
    let file_id = merged.file_id;
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
                "season_number": 1,
                "episode_number": 2,
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

/// R07: with E02 live on `dev_tv`, a no-ID late E01 Progress/Stop from the same
/// device names the same physical file but another unit: it must write nothing.
#[tokio::test]
async fn merged_no_id_other_unit_is_ignored() {
    let fix = PresenceFixture::new();
    let merged = merged_e02_fixture(&fix);
    merged_e02_open_episode_sessions(&fix, &merged).await;
    let read = |season, episode| {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .unit_state(merged.user_id, merged.media_id, season, episode)
            .unwrap()
    };
    let (e01_before, e02_before) = (read(1, 1), read(1, 2));
    for event in ["progress", "stop"] {
        let (status, _) = fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&fix.admin_token),
                json!({
                    "device_id": "dev_tv",
                    "media_item_id": merged.media_id.to_string(),
                    "file_id": merged.file_id.to_string(),
                    "season_number": 1,
                    "episode_number": 1,
                    "event": event,
                    "position_ms": 5000,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(
        read(1, 1),
        e01_before,
        "E01 must not move through the E02 session"
    );
    assert_eq!(read(1, 2), e02_before);
}
