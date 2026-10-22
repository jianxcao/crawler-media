//! Risk 2: a cross-Library re-parent must carry the user's playback summary to the
//! destination Library and drop it from the source, matching N04 sparse semantics
//! (a Library holds a user row only for the units it owns).
use domain::{LibraryId, Media, MediaId, MediaKind, UserId};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    tmp: tempfile::TempDir,
    store: Store,
    libs: Vec<LibraryId>,
    media: MediaId,
    user: UserId,
    moved: domain::LibraryFile,
}

fn show(store: &Store) -> MediaId {
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Show".into(),
            year: Some(2020),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    media
}

fn fixture(keep_source_episode: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut libs = Vec::new();
    for name in ["a", "b"] {
        let root = tmp.path().join(name);
        std::fs::create_dir_all(root.join("Show")).unwrap();
        let lib = store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        libs.push(lib.id.parse::<LibraryId>().unwrap());
    }
    let media = show(&store);
    let mut episodes = vec![("a", 2, 1)];
    if keep_source_episode {
        episodes.push(("a", 1, 1));
    }
    let mut moved = None;
    for (dir, season, episode) in episodes {
        let path = tmp
            .path()
            .join(dir)
            .join(format!("Show/S0{season}E0{episode}.mkv"));
        std::fs::write(&path, b"x").unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(episode),
                ..Default::default()
            },
        )
        .unwrap();
        if season == 2 {
            moved = Some(file);
        }
    }
    let user = UserId::new();
    store
        .set_unit_favorite(user, media, 2, 1, true, 10)
        .unwrap();
    Fx {
        tmp,
        store,
        libs,
        media,
        user,
        moved: moved.unwrap(),
    }
}

fn reparent(fx: &Fx) {
    let to: PathBuf = fx.tmp.path().join("b/Show/S02E01.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.moved.file_id, &fx.moved.version, &fx.moved.path, &to, 1)
        .unwrap();
    fx.store.apply_file_rename_disk(&req.rename_id, 2).unwrap();
    fx.store.complete_file_rename(&req.rename_id, 3).unwrap();
}

fn user_row(fx: &Fx, lib: LibraryId) -> Option<(i64, i64)> {
    let db = Connection::open(fx.tmp.path().join("data/library.db")).unwrap();
    db.query_row(
        "SELECT favorite, owned_episode_count FROM user_library_items
         WHERE library_id = ?1 AND media_id = ?2 AND user_id = ?3",
        params![lib.to_string(), fx.media.to_string(), fx.user.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn integrity_clean(fx: &Fx, lib: LibraryId, op: &str) {
    fx.store
        .begin_library_integrity(lib, false, op, 2000)
        .unwrap();
    for step in 0..50 {
        let status = fx
            .store
            .advance_library_operation(op, 2001 + step, 1)
            .unwrap();
        if status.state != "applying" {
            let report = fx.store.library_integrity_report(op).unwrap();
            assert!(!report.has_issues(), "{op}: {report:?}");
            return;
        }
    }
    panic!("integrity did not finish");
}

#[test]
fn reparent_moves_sole_summary_to_destination_library() {
    let fx = fixture(false);
    assert_eq!(user_row(&fx, fx.libs[0]).map(|r| r.0), Some(1));
    reparent(&fx);
    assert_eq!(
        user_row(&fx, fx.libs[0]),
        None,
        "source no longer owns the unit"
    );
    assert_eq!(
        user_row(&fx, fx.libs[1]).map(|r| r.0),
        Some(1),
        "summary follows the unit"
    );
    integrity_clean(&fx, fx.libs[0], "a");
    integrity_clean(&fx, fx.libs[1], "b");
}

#[test]
fn reparent_recomputes_both_sides_when_source_keeps_other_units() {
    let fx = fixture(true);
    reparent(&fx);
    assert_eq!(user_row(&fx, fx.libs[1]).map(|r| r.0), Some(1));
    // Source still owns S01E01 (no user facts there): sparse ⇒ no stale row.
    let source = user_row(&fx, fx.libs[0]);
    assert!(
        source.is_none() || source == Some((0, 1)),
        "stale source row: {source:?}"
    );
    integrity_clean(&fx, fx.libs[0], "a");
    integrity_clean(&fx, fx.libs[1], "b");
}

fn issue(fx: &Fx) -> String {
    let unit = domain::UnitKey::new(2, 1);
    fx.store
        .issue_play_session(
            fx.user,
            "dev",
            fx.moved.file_id,
            fx.media,
            unit,
            &fx.moved.version,
            1_000,
        )
        .unwrap()
}

fn progress(fx: &Fx, position_ms: i64) {
    let session_id = issue(fx);
    assert_eq!(
        report(fx, session_id, position_ms),
        store::playback::EventOutcome::Applied
    );
}

fn report(fx: &Fx, session_id: String, position_ms: i64) -> store::playback::EventOutcome {
    use store::playback::{BoundPlaybackEvent, PlaybackEventType};
    let unit = domain::UnitKey::new(2, 1);
    let file = &fx.moved;
    let generation = fx
        .store
        .play_session_binding(fx.user, "dev", &session_id)
        .unwrap()
        .unwrap()
        .generation;
    let event = BoundPlaybackEvent {
        user_id: fx.user,
        device_id: "dev".into(),
        session_id,
        generation,
        event_sequence: Some(1),
        item: domain::TypedItemId::Unit {
            media: fx.media,
            key: unit,
        },
        source_id: domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        },
        source_version: file.version.source_version.clone(),
        position_ms,
        duration_ms: Some(3_600_000),
        audio_track: Some("eng".into()),
        subtitle_track: None,
        paused: false,
        rate_bps: 0,
        client: Some("Web".into()),
        device_name: None,
        client_version: None,
        play_method: None,
        event_type: PlaybackEventType::Progress,
    };
    fx.store.commit_playback_event(&event, 1_000).unwrap()
}

#[test]
fn active_session_survives_reparent_of_unchanged_bytes() {
    let fx = fixture(false);
    let session = issue(&fx);
    reparent(&fx);
    assert_eq!(
        report(&fx, session, 900_000),
        store::playback::EventOutcome::Applied,
        "a path move must not invalidate the session the client is playing"
    );
}

#[test]
fn reparent_keeps_resume_binding_and_summary_resume_pointer() {
    use store::playback::ResumeStatus;
    let fx = fixture(false);
    progress(&fx, 600_000);
    reparent(&fx);
    let item = domain::TypedItemId::Unit {
        media: fx.media,
        key: domain::UnitKey::new(2, 1),
    };
    let res = fx
        .store
        .resolve_playback(fx.user, &item, None, None)
        .unwrap();
    assert_eq!(
        res.status,
        ResumeStatus::Compatible,
        "a path move keeps the same bytes"
    );
    assert_eq!(res.start_ms, 600_000);
    assert_eq!(res.audio_track.as_deref(), Some("eng"));
    let summary = fx
        .store
        .get_user_library_summary(fx.user, fx.libs[1], fx.media)
        .unwrap()
        .expect("destination summary");
    assert_eq!(
        (summary.resume_season, summary.resume_episode),
        (Some(2), Some(1))
    );
    assert!(
        fx.store
            .get_user_library_summary(fx.user, fx.libs[0], fx.media)
            .unwrap()
            .is_none()
    );
    integrity_clean(&fx, fx.libs[0], "a");
    integrity_clean(&fx, fx.libs[1], "b");
}
