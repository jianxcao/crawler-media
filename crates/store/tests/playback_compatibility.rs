mod support;

use std::str::FromStr;

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UserId};
use rusqlite::Connection;
use store::Store;
use store::test_support::FixtureFile;

fn insert_movie(store: &Store, id: MediaId) {
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: format!("Fixture {id}"),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
}

fn publish_unit(
    store: &Store,
    base: &std::path::Path,
    media_id: MediaId,
    season: u32,
    episode: u32,
) {
    let kind = store.get_media(media_id).unwrap().unwrap().kind;
    let root = base.join(kind.as_str());
    std::fs::create_dir_all(&root).unwrap();
    if !store
        .list_library_roots()
        .unwrap()
        .iter()
        .any(|candidate| candidate.path == root && candidate.kind == kind)
    {
        store
            .create_library(
                kind,
                &format!("lib-{}-{}", kind.as_str(), base.display()),
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: root
            .join(format!("{media_id}-s{season}e{episode}.mkv"))
            .display()
            .to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &row).unwrap();
}

#[test]
fn failed_movie_mark_does_not_hide_legacy_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let media = MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media,
            kind: domain::MediaKind::Movie,
            title: "Legacy".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    publish_unit(&store, &tmp.path().join("media"), media, 0, 0);
    store
        .upsert_unit(
            user,
            media,
            0,
            0,
            45000,
            Some(false),
            Some(true),
            Some(90000),
            None,
            None,
            false,
            10,
        )
        .unwrap();
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER review_mark_update_block BEFORE UPDATE ON playback_units BEGIN SELECT RAISE(ABORT,'review mark blocked'); END;
         CREATE TRIGGER review_mark_insert_block BEFORE INSERT ON playback_units BEGIN SELECT RAISE(ABORT,'review mark blocked'); END;",
    )
    .unwrap();
    drop(db);
    let blocked = Store::open(tmp.path()).unwrap();
    assert!(
        blocked
            .mutate_playback(
                media,
                &store::playback::PlaybackMutation::marks(user, -1, -1, Some(true), None),
                20
            )
            .is_err()
    );
    drop(blocked);
    let reopened = Store::open(tmp.path()).unwrap();
    let legacy = reopened.unit_state(user, media, 0, 0).unwrap().unwrap();
    assert_eq!(legacy.position_ms, 45000);
    assert!(legacy.favorite);
    assert!(reopened.unit_state(user, media, -1, -1).unwrap().is_none());
}

#[test]
fn clear_viewing_facts_preserves_preferences_and_other_users_and_time_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let other = UserId::new();
    let media = MediaId::new();
    let outside = MediaId::new();
    insert_movie(&store, media);
    insert_movie(&store, outside);
    let root = tmp.path().join("media");
    publish_unit(&store, &root, media, 1, 1);
    publish_unit(&store, &root, outside, 1, 1);
    for (who, what, at) in [(user, media, 100), (other, media, 100), (user, outside, 10)] {
        store
            .upsert_unit(
                who,
                what,
                1,
                1,
                1000,
                Some(true),
                Some(true),
                Some(120000),
                Some("embedded:1"),
                Some("off"),
                true,
                at,
            )
            .unwrap();
    }
    assert_eq!(store.clear_units(user, None, Some(50)).unwrap(), 1);
    let cleared = store.unit_state(user, media, 1, 1).unwrap().unwrap();
    assert_eq!(
        (cleared.position_ms, cleared.played, cleared.play_count),
        (0, false, 0)
    );
    // Clearing history drops the whole resume package — position, duration, source binding
    // and the track selection that belongs to it (the schema binds tracks to the binding).
    // The favourite is a library decision and survives.
    assert!(cleared.favorite, "the favourite survives clearing history");
    assert_eq!(
        (
            cleared.audio_track.as_deref(),
            cleared.subtitle_track.as_deref(),
            cleared.duration_ms
        ),
        (None, None, None),
        "the resume package goes with the history"
    );
    assert_eq!(
        store
            .unit_state(other, media, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store
            .unit_state(user, outside, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store.user_active_media_ids(user).unwrap(),
        vec![(outside, 10)]
    );
}

#[test]
fn critical_unit_mark_cascade_failure_is_propagated_and_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    insert_movie(&store, media);
    support::publish_file(&store, &tmp.path().join("library"), media, "cascade");
    publish_unit(&store, &tmp.path().join("media"), media, 1, 1);
    for (season, episode) in [(-1, -1), (1, 1)] {
        store
            .upsert_unit(
                user,
                media,
                season,
                episode,
                0,
                None,
                Some(true),
                None,
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_mark BEFORE UPDATE ON playback_units WHEN OLD.season=1 BEGIN SELECT RAISE(ABORT,'mark blocked'); END;").unwrap();
    assert!(
        store
            .mutate_playback(
                media,
                &store::playback::PlaybackMutation::marks(user, -1, -1, None, Some(false)),
                2
            )
            .is_err()
    );
    for (season, episode) in [(-1, -1), (1, 1)] {
        assert!(
            store
                .unit_state(user, media, season, episode)
                .unwrap()
                .unwrap()
                .favorite
        );
    }
}

#[test]
fn retargeted_subscribe_does_not_treat_previous_library_as_owned() {
    use domain::{Confidence, LedgerId, Media, MediaKind, QualitySource};
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = domain::MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "Shared".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let a_root = tmp.path().join("library-a");
    let b_root = tmp.path().join("library-b");
    std::fs::create_dir_all(&a_root).unwrap();
    std::fs::create_dir_all(&b_root).unwrap();
    let a = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[a_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &[b_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.set_default_library(&a.id).unwrap();
    let path = a_root.join("Shared.mkv");
    std::fs::write(&path, b"owned").unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        ..Default::default()
    };
    store::test_support::publish_fixture_file(&store, &row).unwrap();
    let mut subscribe = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media,
        coverage: domain::Coverage::Movie,
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(domain::LibraryId::from_str(&a.id).unwrap()),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(
        None,
        None,
        subscribe::QualityFact {
            score: 100,
            path: Some(path.display().to_string()),
        },
    );
    store.save_subscribe_facts(subscribe.id, &facts).unwrap();
    subscribe.library_id = Some(domain::LibraryId::from_str(&b.id).unwrap());
    store.update_subscribe(&subscribe).unwrap();
    let loaded = store
        .load_library_subscribe_facts(&subscribe, MediaKind::Movie)
        .unwrap();
    assert!(
        loaded.movie().is_none(),
        "Library A 的文件不能算作空 Library B 的已拥有内容"
    );
    assert!(path.is_file(), "切换目标不得删除原 Library 文件");
}

fn closed_session(
    store: &Store,
    user: UserId,
    media: MediaId,
    file: LedgerId,
    device: &str,
    started: i64,
    admin_ended: bool,
) -> store::SessionRow {
    let play_session_id = support::issue_session(store, user, device, file, started);
    store::SessionRow {
        device_id: device.into(),
        user_id: user,
        media_id: media,
        season: Some(-1),
        episode: Some(-1),
        client: Some("fixture".into()),
        device_name: None,
        client_version: None,
        play_method: "local".into(),
        position_ms: 500,
        start_position_ms: 0,
        duration_ms: Some(1000),
        paused: false,
        watched_ms: 10,
        rate_bps: 0,
        bytes_sent: 0,
        connections: 0,
        admin_ended,
        started_at: started,
        last_report_at: started,
        ledger_id: Some(file.to_string()),
        play_session_id: Some(play_session_id),
        last_report_at_ms: started * 1000,
        last_event_sequence: None,
    }
}

/// 只有管理员结束的会话才写 `admin_ended = 1`；用户自行停止必须是 0，
/// 否则迟到的心跳会让前端弹出"管理员已结束播放"。
#[test]
fn only_admin_end_records_admin_ended_in_playback_log() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let file = support::seed_published_file(&store, &tmp.path().join("library"), "admin-end").1;

    store
        .upsert_session(&closed_session(
            &store,
            user,
            media,
            file,
            "dev-user-stop",
            10,
            false,
        ))
        .unwrap();
    store.close_session(user, "dev-user-stop", 20).unwrap();
    store
        .upsert_session(&closed_session(
            &store,
            user,
            media,
            file,
            "dev-admin-end",
            30,
            true,
        ))
        .unwrap();
    store.close_session(user, "dev-admin-end", 40).unwrap();

    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let user_stop: i64 = db
        .query_row(
            "SELECT admin_ended FROM playback_logs WHERE device_id = 'dev-user-stop'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let admin_end: i64 = db
        .query_row(
            "SELECT admin_ended FROM playback_logs WHERE device_id = 'dev-admin-end'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_stop, 0, "用户自行停止不得被记为管理员结束");
    assert_eq!(admin_end, 1, "管理员结束必须落库");

    assert!(
        !store
            .device_was_admin_ended(user, "dev-user-stop", 25, 60)
            .unwrap(),
        "用户停止后的迟到心跳不得被判定为管理员结束"
    );
    assert!(
        store
            .device_was_admin_ended(user, "dev-admin-end", 45, 60)
            .unwrap(),
        "管理员结束后的心跳必须被告知已结束"
    );
}
