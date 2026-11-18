//! Session sequence ordering survives live-row archival without preventing resumed playback.
mod playback_event_support;
use playback_event_support::*;

fn archive(f: &Fixture, sweep: bool, now: i64) {
    if sweep {
        assert_eq!(f.store.sweep_stale_sessions(now, 30).unwrap(), 1);
        assert_eq!(f.store.sweep_stale_sessions(now + 1, 30).unwrap(), 0);
    } else {
        assert!(
            f.store
                .close_session(USER.parse().unwrap(), "device_web", now)
                .unwrap()
                .is_some()
        );
        assert!(
            f.store
                .close_session(USER.parse().unwrap(), "device_web", now + 1)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        f.store
            .get_session(USER.parse().unwrap(), "device_web")
            .unwrap()
            .is_none()
    );
}

fn refuse_old_events(f: &Fixture, bound: &mut BoundPlaybackEvent) {
    for event_type in [
        PlaybackEventType::Playing,
        PlaybackEventType::Progress,
        PlaybackEventType::Stopped,
    ] {
        for sequence in [2, 3] {
            bound.event_type = event_type;
            bound.event_sequence = Some(sequence);
            bound.position_ms = Some(2_300_000);
            assert_eq!(
                f.store.commit_playback_event(bound, 102_000).unwrap(),
                EventOutcome::IgnoredStale
            );
        }
    }
}

fn archived_session_retains_sequence(sweep: bool) {
    let (_tmp, f) = seed();
    let user = USER.parse().unwrap();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item.clone(),
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    assert_eq!(
        f.store.commit_playback_event(&bound, 2_000).unwrap(),
        EventOutcome::Applied
    );
    let next = issue(&f, &item, &f.file);
    archive(&f, sweep, 100);
    assert_eq!(
        f.store
            .play_session_binding(user, "device_web", &session.0)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(3)
    );
    let before_unit = f.store.unit_state(user, f.media_id, 1, 1).unwrap();
    refuse_old_events(&f, &mut bound);
    assert_eq!(
        f.store.unit_state(user, f.media_id, 1, 1).unwrap(),
        before_unit
    );
    assert!(f.store.get_session(user, "device_web").unwrap().is_none());
    let logs = f
        .store
        .list_logs(10, None, None, None, 103, Some(user))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].end_position_ms, 6_000);
    bound.event_type = PlaybackEventType::Progress;
    bound.event_sequence = None;
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 104_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(7_000));
    archive(&f, sweep, 200);
    bound.event_sequence = Some(3);
    assert_eq!(
        f.store.commit_playback_event(&bound, 204_000).unwrap(),
        EventOutcome::IgnoredStale,
        "an unsequenced resume must not erase the persisted sequence watermark"
    );
    bound.event_sequence = Some(4);
    bound.position_ms = Some(8_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 205_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(8_000));
    let binding = f
        .store
        .play_session_binding(user, "device_web", &session.0)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Active);
    assert_eq!(binding.last_event_sequence, Some(4));
    let next_binding = f
        .store
        .play_session_binding(user, "device_web", &next.0)
        .unwrap()
        .unwrap();
    assert_eq!(next_binding.state, PlaySessionIdState::Pending);
    let mut fresh = event(
        &f,
        item,
        (&next.0, next.1),
        &f.file,
        9_000,
        PlaybackEventType::Playing,
    );
    fresh.event_sequence = Some(1);
    assert_eq!(
        f.store.commit_playback_event(&fresh, 206_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(9_000));
}

#[test]
fn closed_live_session_rejects_old_sequences_and_resumes_new_events() {
    archived_session_retains_sequence(false);
}

#[test]
fn swept_live_session_rejects_old_sequences_and_resumes_new_events() {
    archived_session_retains_sequence(true);
}

#[test]
fn failed_archive_keeps_the_binding_live_with_its_session() {
    let (tmp, f) = seed();
    let user = USER.parse().unwrap();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item,
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    f.store.commit_playback_event(&bound, 2_000).unwrap();
    let before = f.store.get_session(user, "device_web").unwrap();
    // Inject an archive failure; all observations below go through the Store API.
    let diagnostic = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    diagnostic
        .execute_batch(
            "CREATE TRIGGER fail_playback_archive BEFORE INSERT ON playback_logs
         BEGIN SELECT RAISE(FAIL, 'archive fault'); END;",
        )
        .unwrap();
    assert!(f.store.close_session(user, "device_web", 100).is_err());
    assert_eq!(f.store.get_session(user, "device_web").unwrap(), before);
    let binding = f
        .store
        .play_session_binding(user, "device_web", &session.0)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Active);
    assert_eq!(binding.last_event_sequence, Some(3));
    assert!(
        f.store
            .list_logs(10, None, None, None, 101, Some(user))
            .unwrap()
            .is_empty()
    );
    bound.event_sequence = Some(4);
    bound.position_ms = Some(7_000);
    assert_eq!(
        f.store.commit_playback_event(&bound, 102_000).unwrap(),
        EventOutcome::Applied
    );
}

#[test]
fn failed_event_transaction_does_not_advance_binding_sequence() {
    let (tmp, f) = seed();
    let user = USER.parse().unwrap();
    let item = episode(&f, 1);
    let session = issue(&f, &item, &f.file);
    let mut bound = event(
        &f,
        item,
        (&session.0, session.1),
        &f.file,
        6_000,
        PlaybackEventType::Progress,
    );
    bound.event_sequence = Some(3);
    f.store.commit_playback_event(&bound, 2_000).unwrap();
    let before = f.store.get_session(user, "device_web").unwrap();
    let diagnostic = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    diagnostic
        .execute_batch(
            "CREATE TRIGGER fail_playback_unit BEFORE UPDATE ON playback_units
         BEGIN SELECT RAISE(FAIL, 'unit fault'); END;",
        )
        .unwrap();
    bound.event_sequence = Some(4);
    bound.position_ms = Some(7_000);
    assert!(f.store.commit_playback_event(&bound, 3_000).is_err());
    assert_eq!(f.store.get_session(user, "device_web").unwrap(), before);
    assert_eq!(
        f.store
            .play_session_binding(user, "device_web", &session.0)
            .unwrap()
            .unwrap()
            .last_event_sequence,
        Some(3)
    );
    diagnostic
        .execute_batch("DROP TRIGGER fail_playback_unit;")
        .unwrap();
    assert_eq!(
        f.store.commit_playback_event(&bound, 4_000).unwrap(),
        EventOutcome::Applied
    );
    assert_eq!(unit_position(&f, 1), Some(7_000));
}

fn seed_loser_show(
    store: &store::Store,
    library_id: &str,
    root_id: &str,
    loser_id: MediaId,
) -> LibraryFile {
    store
        .insert_media(&Media {
            id: loser_id,
            kind: MediaKind::Tv,
            title: "Loser Show".into(),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        })
        .unwrap();
    let loser_file = LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: library_id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id: loser_id,
        kind: MediaKind::Tv,
        path: "Loser Show/S01E01.mkv".into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash2".into()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(1_800_000),
        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library_id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &loser_file,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    loser_file
}

#[test]
fn library_removal_and_session_sweep_succeed_after_media_merge() {
    let (_tmp, f) = seed();
    let user = USER.parse().unwrap();
    let loser_id = MediaId::new();
    let loser_file = seed_loser_show(&f.store, &f.library_id, &f.root_id, loser_id);

    // Start two active playback sessions on the loser Media before merging:
    // one stale (for sweep) and one fresh (for Library removal).
    let loser_item = TypedItemId::Unit {
        media: loser_id,
        key: UnitKey::new(1, 1),
    };
    for (device, report_at_ms) in [("dev_stale", 1_000_i64), ("dev_active", 99_995_000_i64)] {
        let sid = f
            .store
            .issue_play_session(
                user,
                device,
                loser_file.file_id,
                loser_id,
                UnitKey::new(1, 1),
                &loser_file.version,
                1_000,
            )
            .unwrap();
        let generation = f
            .store
            .play_session_binding(user, device, &sid)
            .unwrap()
            .unwrap()
            .generation;
        let mut bound = event_on(
            &f,
            device,
            loser_item.clone(),
            (&sid, generation),
            &loser_file,
            5_000,
            PlaybackEventType::Playing,
        );
        bound.event_sequence = Some(1);
        assert_eq!(
            f.store.commit_playback_event(&bound, report_at_ms).unwrap(),
            EventOutcome::Applied
        );
    }

    // Merge loser into survivor (f.media_id) while both sessions exist.
    let op_id = f
        .store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                survivor: f.media_id,
                loser: loser_id,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: std::collections::BTreeMap::new(),
            },
            1_000,
        )
        .unwrap();
    while f.store.advance_media_merge(&op_id, 1_000).unwrap().state != "succeeded" {}

    // Sweep stale sessions (closes dev_stale) and Library removal (closes dev_active) must both succeed cleanly!
    assert_eq!(f.store.sweep_stale_sessions(100_000, 30).unwrap(), 1);
    assert!(f.store.get_session(user, "dev_stale").unwrap().is_none());
    assert!(f.store.get_session(user, "dev_active").unwrap().is_some());

    let extra_lib = f
        .store
        .create_library(
            MediaKind::Tv,
            "Other TV",
            &["/other-tv"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert!(!extra_lib.id.is_empty());
    let removal = f.store.begin_library_removal(&f.library_id).unwrap();
    assert_eq!(
        f.store
            .close_playback_sessions_for_removal(&removal.operation_id)
            .unwrap(),
        1
    );
    f.store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    let final_op = f
        .store
        .clean_library_removal(&removal.operation_id)
        .unwrap();
    assert_eq!(final_op.state, "succeeded");
}
