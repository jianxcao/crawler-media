//! Invariants the summary model must hold, exercised through the public Store seam.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media,
    MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn media(store: &Store, id: &str, title: &str, kind: MediaKind) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media
}

fn library(store: &Store, name: &str, kind: MediaKind, root: &str) -> (store::Library, String) {
    let lib = store
        .create_library(kind, name, &[root], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id == lib.id.clone())
        .map(|candidate| candidate.id)
        .unwrap();
    (lib, root_id)
}

fn file(
    file_id: &str,
    lib: &store::Library,
    root_id: &str,
    media: &Media,
    path: &str,
) -> LibraryFile {
    LibraryFile {
        file_id: file_id.parse().unwrap(),
        library_id: lib.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id: media.id,
        kind: media.kind,
        path: path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(10),
        modified_ns: Some(20),
        source_digest: None,
        source_path: Some("/downloads/source.mkv".into()),
        transfer_mode: Some("hardlink".into()),
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: Some("HDR10".into()),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(1_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

/// A pending file is a management fact, not library content: it must not create a card.
#[test]
fn pending_files_do_not_create_cards_or_size() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(
        &store,
        "00000000-0000-0000-0000-000000000001",
        "Pending",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let mut pending = file(
        "00000000-0000-0000-0001-000000000001",
        &lib,
        &root_id,
        &media,
        "/movies/Pending.mkv",
    );
    pending.publication = FilePublication::Pending;
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &pending,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    assert!(
        store
            .get_library_item(scope.library_id, media.id)
            .unwrap()
            .is_none(),
        "a pending-only library must not show a card"
    );

    pending.publication = FilePublication::Published;
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &pending,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            2_000,
        )
        .unwrap();
    let item = store
        .get_library_item(scope.library_id, media.id)
        .unwrap()
        .expect("published file must create the card");
    assert_eq!(item.file_count, 1);
    assert_eq!(item.available_file_count, 1);
    assert_eq!(item.size_bytes, Some(1_000));
}

/// Publication into a Library that is being removed must fail instead of writing.
#[test]
fn frozen_library_rejects_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(
        &store,
        "00000000-0000-0000-0000-000000000002",
        "Frozen",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let _spare = library(&store, "Movies 2", MediaKind::Movie, "/movies2");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    store.begin_library_removal(&lib.id).unwrap();

    let file = file(
        "00000000-0000-0000-0001-000000000002",
        &lib,
        &root_id,
        &media,
        "/movies/Frozen.mkv",
    );
    let error = store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .expect_err("a frozen library must reject publication");
    assert!(
        matches!(error, store::StoreError::Protected(_)),
        "{error:?}"
    );
}

/// A path outside the declared root is rejected instead of being written.
#[test]
fn publication_rejects_path_outside_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(
        &store,
        "00000000-0000-0000-0000-000000000003",
        "Outside",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = file(
        "00000000-0000-0000-0001-000000000003",
        &lib,
        &root_id,
        &media,
        "/elsewhere/Outside.mkv",
    );
    assert!(
        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1_000,
            )
            .is_err()
    );
}

/// Scan ownership is decided by path components: the longest root that actually
/// contains the file wins, and a sibling directory with a longer name does not.
#[test]
fn scan_ownership_prefers_real_component_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(
        &store,
        "00000000-0000-0000-0000-000000000004",
        "Nested",
        MediaKind::Movie,
    );
    let (base, _) = library(&store, "Base", MediaKind::Movie, "/media");
    let (nested, _) = library(&store, "Nested", MediaKind::Movie, "/media/Show");

    let file_id = LedgerId::new();
    let row = store::ScannedLedger {
        file_id,
        media_id: media.id,
        path: "/media/Showcase/movie.mkv".into(),
        units: vec![UnitKey::WHOLE],
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        source_version: "fixture-v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        artwork: Default::default(),
        observations: Vec::new(),
    };
    store.insert_scanned_files(&[row], 1_000).unwrap();

    let (_, owner) = store.get_file_path_and_library(file_id).unwrap().unwrap();
    assert_eq!(
        owner, base.id,
        "a textual prefix must not steal a file from a sibling directory"
    );
    assert_ne!(owner, nested.id);
}

/// Reassigning a published file rebuilds both summaries inside the transaction.
#[test]
fn rebind_moves_the_file_and_rebuilds_both_cards() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let source = media(
        &store,
        "00000000-0000-0000-0000-000000000005",
        "Source",
        MediaKind::Movie,
    );
    let target = media(
        &store,
        "00000000-0000-0000-0000-000000000006",
        "Target",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = file(
        "00000000-0000-0000-0001-000000000005",
        &lib,
        &root_id,
        &source,
        "/movies/Rebind.mkv",
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();

    store
        .rebind_scanned_ledger(file.file_id, target.id)
        .unwrap();

    assert!(
        store
            .get_library_item(scope.library_id, source.id)
            .unwrap()
            .is_none(),
        "the previous media must lose its card"
    );
    let moved = store
        .get_library_item(scope.library_id, target.id)
        .unwrap()
        .expect("the new media must gain the card");
    assert_eq!(moved.file_count, 1);
    assert_eq!(moved.preferred_file_id, Some(file.file_id));
}

/// A publication that only repeats the same facts must not advance the file revision,
/// so a resume binding stays valid.
#[test]
fn republishing_identical_facts_keeps_the_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(
        &store,
        "00000000-0000-0000-0000-000000000007",
        "Stable",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = file(
        "00000000-0000-0000-0001-000000000007",
        &lib,
        &root_id,
        &media,
        "/movies/Stable.mkv",
    );
    let publication = || StorePublication {
        file: &file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    store
        .publish_files(&scope, &[publication()], 1_000)
        .unwrap();
    let report = store
        .publish_files(&scope, &[publication()], 2_000)
        .expect("republishing identical facts must succeed");
    assert_eq!(report.unchanged, 1);
    assert_eq!(report.updated, 0);
    assert!(store.get_file_details(file.file_id).unwrap().is_none());
}

#[test]
fn incremental_episode_add_refreshes_existing_user_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let show = media(
        &store,
        "00000000-0000-0000-0000-000000000008",
        "Series",
        MediaKind::Tv,
    );
    let (lib, root_id) = library(&store, "TV", MediaKind::Tv, "/tv");
    let lib_id = lib.id.parse::<domain::LibraryId>().unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };

    // First publish Episode 1
    let file1 = file(
        "00000000-0000-0000-0001-000000000008",
        &lib,
        &root_id,
        &show,
        "/tv/Series/S01E01.mkv",
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file1,
                units: &[UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();

    let user_id = domain::UserId::new();
    // Mark Episode 1 as played
    store
        .mutate_playback(
            show.id,
            &store::playback::PlaybackMutation::marks(user_id, 1, 1, Some(true), None),
            2_000,
        )
        .unwrap();

    let summary_before = store
        .get_user_library_summary(user_id, lib_id, show.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            summary_before.owned_episode_count,
            summary_before.completed_episode_count
        ),
        (1, 1)
    );
    assert!(summary_before.played);

    // Now publish Episode 2 incrementally (single file publish)
    let file2 = file(
        "00000000-0000-0000-0001-000000000009",
        &lib,
        &root_id,
        &show,
        "/tv/Series/S01E02.mkv",
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file2,
                units: &[UnitKey::new(1, 2)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            3_000,
        )
        .unwrap();

    let summary = store
        .get_user_library_summary(user_id, lib_id, show.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (summary.owned_episode_count, summary.completed_episode_count),
        (2, 1)
    );
    assert_eq!(
        (summary.next_season, summary.next_episode),
        (Some(1), Some(2))
    );
    assert!(!summary.played);
}

#[test]
fn reimport_restores_sparse_favorites() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(
        &store,
        "00000000-0000-0000-0000-000000000009",
        "Favorite Movie",
        MediaKind::Movie,
    );
    let (lib, root_id) = library(&store, "Movies", MediaKind::Movie, "/movies");
    let lib_id = lib.id.parse::<domain::LibraryId>().unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };

    let file1 = file(
        "00000000-0000-0000-0001-000000000010",
        &lib,
        &root_id,
        &movie,
        "/movies/fav.mkv",
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file1,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();

    let user_id = domain::UserId::new();
    // Mark as favorite
    store
        .mutate_playback(
            movie.id,
            &store::playback::PlaybackMutation::marks(
                user_id,
                store::UNIT_WHOLE,
                store::UNIT_WHOLE,
                None,
                Some(true),
            ),
            2_000,
        )
        .unwrap();

    let sum1 = store
        .get_user_library_summary(user_id, lib_id, movie.id)
        .unwrap()
        .unwrap();
    assert!(sum1.favorite);

    // Delete the file
    store.delete_files(lib_id, &[file1.file_id]).unwrap();

    // After deleting last file, user_library_items row is cleared
    let sum2 = store
        .get_user_library_summary(user_id, lib_id, movie.id)
        .unwrap();
    assert!(sum2.is_none());

    // Reimport file
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file1,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            4_000,
        )
        .unwrap();

    // Restores favorite state
    let sum3 = store
        .get_user_library_summary(user_id, lib_id, movie.id)
        .unwrap()
        .unwrap();
    assert!(sum3.favorite);
}
