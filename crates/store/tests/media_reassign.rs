//! Task 15 (store half): reassignment must move *exactly* the files the caller listed,
//! at the version the caller pinned, and must not rewrite the source Media's identity.
//!
//! `// reassignment_is_scoped_and_merge_assertion_rule_is_deterministic`

use std::path::{Path, PathBuf};

use domain::{
    Confidence, LedgerId, LibraryFile, Media, MediaId, MediaKind, PhysicalSourceId, QualitySource,
    UnitKey, UserId,
};
use store::library_write::ReassignFiles;
use store::playback::MediaMergeRequest;
use store::test_support::FixtureFile;
use store::{PlaySessionIdState, SessionRow, Store, StoreError};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    root: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path().join("data")).expect("open store");
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).expect("create root");
    let root = std::fs::canonicalize(&root).expect("canonicalize root");
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().expect("utf-8 root")],
            "everyone",
            true,
            &[],
        )
        .expect("create movie library");
    Fixture {
        _dir: dir,
        store,
        root,
    }
}

fn media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind,
            title: title.to_string(),
            year: Some(2024),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: Vec::new(),
        })
        .expect("insert media");
    id
}

/// Publish one file for `media_id` under `root`, optionally covering a TV unit.
fn publish(
    store: &Store,
    root: &Path,
    media_id: MediaId,
    name: &str,
    unit: Option<UnitKey>,
) -> LedgerId {
    let path = root.join(format!("{name}.mkv"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create file parent");
    }
    std::fs::write(&path, b"fixture").expect("write fixture file");
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id,
        path: path.to_string_lossy().into_owned(),
        season: unit.map(|u| u.season.max(0) as u32),
        episode: unit.map(|u| u.episode.max(0) as u32),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &row).expect("publish fixture file");
    row.file_id.expect("fixture file id")
}

fn file(store: &Store, id: LedgerId) -> LibraryFile {
    store
        .published_files(&[id])
        .expect("read published file")
        .remove(&id)
        .unwrap_or_else(|| panic!("fixture file {id} is not published"))
}

fn source_of(store: &Store, id: LedgerId) -> PhysicalSourceId {
    let found = file(store, id);
    PhysicalSourceId {
        file_id: id,
        revision: found.version.revision,
    }
}

fn revision(store: &Store, id: MediaId) -> i64 {
    store
        .media_revision(id)
        .expect("read media revision")
        .expect("media exists")
}

fn issue_session(store: &Store, user: UserId, device: &str, id: LedgerId, now: i64) -> String {
    let found = file(store, id);
    let unit = store
        .get_file_units(id)
        .expect("read units")
        .first()
        .copied()
        .unwrap_or(UnitKey::WHOLE);
    let play_session_id = store
        .issue_play_session(user, device, id, found.media_id, unit, &found.version, now)
        .expect("issue play session");
    // A live session is the issued id *plus* the report row a heartbeat writes;
    // reassignment can only end sessions that are really live.
    store
        .upsert_session(&SessionRow {
            device_id: device.into(),
            user_id: user,
            media_id: found.media_id,
            season: Some(unit.season),
            episode: Some(unit.episode),
            client: Some("fixture".into()),
            device_name: None,
            client_version: None,
            play_method: "local".into(),
            position_ms: now,
            start_position_ms: 0,
            duration_ms: Some(6_000),
            paused: false,
            watched_ms: now,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 1,
            admin_ended: false,
            started_at: now,
            last_report_at: now,
            ledger_id: Some(id.to_string()),
            play_session_id: Some(play_session_id.clone()),
            last_report_at_ms: now * 1000,
            last_event_sequence: None,
        })
        .expect("report live session");
    play_session_id
}

/// The plan assertion: only listed files move, and the source Media keeps its user facts.
#[test]
fn reassignment_moves_only_listed_files_and_preserves_source_facts() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let keep = publish(store, &fx.root, a, "keep", None);
    let moved = publish(store, &fx.root, a, "move", None);
    let user = UserId::new();

    // A's watch history and resume binding live on the *unlisted* sibling file.
    store
        .upsert_unit_for_source(
            user,
            a,
            UnitKey::WHOLE,
            source_of(store, keep),
            5_000,
            Some(true),
            Some(true),
            Some(7_200_000),
            None,
            None,
            true,
            10,
        )
        .expect("seed source facts");
    let facts_before = store.playback_units_for_user(user).expect("read facts");

    let count = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(moved, file(store, moved).version)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect("reassign listed file");
    assert_eq!(count, 1);

    assert_eq!(file(store, moved).media_id, b, "listed file changes owner");
    assert_eq!(
        file(store, moved).library_id,
        file(store, keep).library_id,
        "library never changes"
    );
    assert_eq!(
        file(store, keep).media_id,
        a,
        "unlisted sibling must not move"
    );
    assert_eq!(
        store.playback_units_for_user(user).expect("read facts"),
        facts_before
    );
    let a_state = store
        .unit_state(user, a, -1, -1)
        .expect("read A unit")
        .expect("A unit exists");
    assert_eq!(a_state.position_ms, 5_000, "A keeps its resume binding");
    assert!(
        store
            .unit_state(user, b, -1, -1)
            .expect("read B unit")
            .is_none(),
        "B gains no facts"
    );
}

/// Version and revision CAS: a caller reading a stale row must be told, not guessed at.
#[test]
fn stale_versions_and_revisions_reject_without_moving_anything() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let keep = publish(store, &fx.root, a, "keep", None);
    let moved = publish(store, &fx.root, a, "move", None);

    let mut stale = file(store, moved).version;
    stale.source_version = format!("{}_stale", stale.source_version);
    let err = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(moved, stale)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect_err("stale source_version must be rejected");
    assert!(matches!(err, StoreError::Protected(_)), "got {err:?}");
    assert_eq!(
        file(store, moved).media_id,
        a,
        "rejected request moves nothing"
    );

    let err = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(moved, file(store, moved).version)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a) + 7,
            },
            20,
        )
        .expect_err("stale media revision must be rejected");
    assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
    assert_eq!(file(store, moved).media_id, a);
    assert_eq!(file(store, keep).media_id, a);
}

/// Replaying a settled request is a conflict, never a second silent move.
#[test]
fn replayed_request_conflicts_and_never_moves_twice() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let moved = publish(store, &fx.root, a, "move", None);
    let request = ReassignFiles {
        files: vec![(moved, file(store, moved).version)],
        from: a,
        to: b,
        expected_media_revision: revision(store, a),
    };
    assert_eq!(
        store.reassign_files(&request, 20).expect("first reassign"),
        1
    );

    let err = store
        .reassign_files(&request, 21)
        .expect_err("replay must not re-move");
    assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
    assert_eq!(file(store, moved).media_id, b);
}

/// Re-identification ends live playback bound to the files that changed owner.
#[test]
fn reassignment_ends_bound_sessions_and_keeps_the_history_binding() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let moved = publish(store, &fx.root, a, "move", None);
    let user = UserId::new();
    store
        .upsert_unit_for_source(
            user,
            a,
            UnitKey::WHOLE,
            source_of(store, moved),
            5_000,
            Some(true),
            None,
            None,
            None,
            None,
            true,
            10,
        )
        .expect("seed resume on moved file");
    let session = issue_session(store, user, "dev-1", moved, 10);
    assert_eq!(
        store
            .sessions_for_device("dev-1")
            .expect("live sessions")
            .len(),
        1
    );

    store
        .reassign_files(
            &ReassignFiles {
                files: vec![(moved, file(store, moved).version)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect("reassign listed file");

    assert!(
        store
            .sessions_for_device("dev-1")
            .expect("live sessions")
            .is_empty(),
        "live session ends"
    );
    let binding = store
        .play_session_binding(user, "dev-1", &session)
        .expect("read session id")
        .expect("session id is kept for audit");
    assert_eq!(binding.state, PlaySessionIdState::Closed);
    assert_eq!(
        binding.ledger_id, moved,
        "history keeps its original binding"
    );
    assert_eq!(binding.file_id.as_deref(), Some(moved.to_string().as_str()));
    let state = store
        .unit_state(user, a, -1, -1)
        .expect("read A unit")
        .expect("A unit exists");
    assert_eq!(state.position_ms, 0, "stale resume pointer is dropped");
    assert_eq!(state.play_count, 1, "watch history survives");
}

/// Validation and compatibility guards.
#[test]
fn self_and_mismatched_requests_are_rejected() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let file_a = publish(store, &fx.root, a, "a1", None);
    let version = file(store, file_a).version;

    let same = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(file_a, version.clone())],
                from: a,
                to: a,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect_err("cannot reassign onto itself");
    assert!(matches!(same, StoreError::Invalid(_)), "got {same:?}");

    let empty = store
        .reassign_files(
            &ReassignFiles {
                files: Vec::new(),
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect_err("empty list must be rejected");
    assert!(matches!(empty, StoreError::Invalid(_)), "got {empty:?}");

    let duplicated = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(file_a, version.clone()), (file_a, version)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect_err("duplicate file must be rejected");
    assert!(
        matches!(duplicated, StoreError::Invalid(_)),
        "got {duplicated:?}"
    );

    let unlisted = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(LedgerId::new(), file(store, file_a).version)],
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect_err("unknown file must be rejected");
    assert!(
        matches!(unlisted, StoreError::Missing(_)),
        "got {unlisted:?}"
    );
    assert_eq!(file(store, file_a).media_id, a);
}

/// A file keeps the kind of its owning Media: cross-kind moves are refused.
#[test]
fn cross_kind_reassignment_is_refused() {
    let fx = fixture();
    let store = &fx.store;
    let tv_root = fx.root.parent().expect("parent").join("series");
    std::fs::create_dir_all(&tv_root).expect("create tv root");
    let tv_root = std::fs::canonicalize(&tv_root).expect("canonicalize tv root");
    store
        .create_library(
            MediaKind::Tv,
            "Series",
            &[tv_root.to_str().expect("utf-8 root")],
            "everyone",
            true,
            &[],
        )
        .expect("create tv library");
    let series = media(store, MediaKind::Tv, "S");
    let movie = media(store, MediaKind::Movie, "A");
    publish(store, &fx.root, movie, "a1", None);
    let episode = publish(store, &tv_root, series, "s01e01", Some(UnitKey::new(1, 1)));

    let err = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(episode, file(store, episode).version)],
                from: series,
                to: movie,
                expected_media_revision: revision(store, series),
            },
            20,
        )
        .expect_err("episode file cannot join a movie");
    assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
    assert_eq!(file(store, episode).media_id, series);
}

/// A merging/retired identity must not accept new references.
#[test]
fn retired_merge_loser_cannot_receive_files() {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let keeper = media(store, MediaKind::Movie, "Keeper");
    let donor = media(store, MediaKind::Movie, "Donor");
    publish(store, &fx.root, a, "a1", None);
    publish(store, &fx.root, keeper, "k1", None);
    let donor_file = publish(store, &fx.root, donor, "d1", None);

    let op = store
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                survivor: keeper,
                loser: a,
                survivor_revision: revision(store, keeper),
                loser_revision: revision(store, a),
                refresh_targets: Default::default(),
            },
            10,
        )
        .expect("begin merge");
    let status = store.advance_media_merge(&op, 11).expect("advance merge");
    assert_eq!(
        status.state, "succeeded",
        "fixture merge must settle: {status:?}"
    );

    let err = store
        .reassign_files(
            &ReassignFiles {
                files: vec![(donor_file, file(store, donor_file).version)],
                from: donor,
                to: a,
                expected_media_revision: revision(store, donor),
            },
            12,
        )
        .expect_err("retired loser must reject new files");
    assert!(matches!(err, StoreError::Protected(_)), "got {err:?}");
    let message = format!("{err:?}");
    assert!(
        message.contains(&keeper.to_string()),
        "retired loser refusal must name the survivor {keeper}: {message}"
    );
    assert_eq!(file(store, donor_file).media_id, donor);
    assert_ne!(
        file(store, donor_file).media_id,
        keeper,
        "the refusal must not silently redirect the file onto the survivor"
    );
}

/// The observable result of one reassignment run, comparable across fixtures.
#[derive(Debug, PartialEq)]
struct ReassignOutcome {
    moved: usize,
    owners_moved: bool,
    same_library: bool,
    source_revision: i64,
    target_revision: i64,
    moved_versions: Vec<i64>,
}

/// Task 15 step 4: the policy result must not depend on the order of the caller's
/// explicit snapshot. Listing two files forward and reversed must land exactly
/// the same ownership, versions and summaries.
#[test]
fn reassignment_result_is_independent_of_the_listed_order() {
    let forward = reassign_two_files(false);
    let reversed = reassign_two_files(true);
    assert_eq!(
        forward, reversed,
        "swapping the listed snapshot order changed the result"
    );
    assert_eq!(forward.moved, 2, "every listed file moves");
    assert!(forward.owners_moved, "both files belong to the target");
    assert!(
        forward.same_library,
        "reassignment never changes the library"
    );
}

fn reassign_two_files(reversed: bool) -> ReassignOutcome {
    let fx = fixture();
    let store = &fx.store;
    let a = media(store, MediaKind::Movie, "A");
    let b = media(store, MediaKind::Movie, "B");
    let one = publish(store, &fx.root, a, "one", None);
    let two = publish(store, &fx.root, a, "two", None);
    let mut files = vec![
        (one, file(store, one).version),
        (two, file(store, two).version),
    ];
    if reversed {
        files.reverse();
    }
    let moved = store
        .reassign_files(
            &ReassignFiles {
                files,
                from: a,
                to: b,
                expected_media_revision: revision(store, a),
            },
            20,
        )
        .expect("reassign both listed files");
    ReassignOutcome {
        moved,
        owners_moved: [one, two].iter().all(|id| file(store, *id).media_id == b),
        same_library: file(store, one).library_id == file(store, two).library_id,
        source_revision: revision(store, a),
        target_revision: revision(store, b),
        moved_versions: [one, two]
            .iter()
            .map(|id| file(store, *id).version.revision)
            .collect(),
    }
}
