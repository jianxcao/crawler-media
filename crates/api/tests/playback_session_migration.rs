use std::str::FromStr;
use store::test_support::{publish_fixture_file, FixtureFile};

#[path = "support/mod.rs"]
mod support;

use api::Store;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UserId};

/// Sessions are user-scoped: the library layout keys `playback_sessions` by
/// `(user_id, device_id)`, so two users sharing one device id never collide.
#[test]
fn legacy_device_primary_key_migrates_to_user_scoped_key() {
    let dir = tempfile::tempdir().unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let media = MediaId::from_str("00000000-0000-0000-0000-000000000002").unwrap();
    let library_root = dir.path().join("media");
    std::fs::create_dir_all(&library_root).unwrap();
    let path = library_root.join("movie.mkv");
    std::fs::write(&path, b"x").unwrap();
    {
        let store = Store::open(dir.path()).unwrap();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: "Migration Fixture".into(),
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
        store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[library_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let ledger = LedgerId::new();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(ledger),
                media_id: media,
                path: path.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                ..Default::default()
            },
        )
        .unwrap();
        let play_session_id =
            support::session::issue_session(&store, user, "living-room", ledger, 1);
        store
            .upsert_session(&api::store::SessionRow {
                device_id: "living-room".into(),
                user_id: user,
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
                connections: 0,
                admin_ended: false,
                started_at: 1,
                last_report_at: 1,
                ledger_id: Some(ledger.to_string()),
                play_session_id: Some(play_session_id),
                last_report_at_ms: 1000,
                last_event_sequence: None,
            })
            .unwrap();
    }

    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let mut primary_key: Vec<(i64, String)> = db
        .prepare("PRAGMA table_info(playback_sessions)")
        .unwrap()
        .query_map([], |row| Ok((row.get(5)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .filter(|(position, _)| *position > 0)
        .collect();
    primary_key.sort_by_key(|(position, _)| *position);
    assert_eq!(
        primary_key,
        vec![(1, "user_id".into()), (2, "device_id".into())]
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM playback_sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
}
