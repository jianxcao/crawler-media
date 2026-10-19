//! Invariants the summary model must hold, exercised through the public Store seam.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LedgerRow, LibraryFile,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

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
        .find(|candidate| candidate.library_id == Some(lib.id.clone()))
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
    let media = media(&store, "00000000-0000-0000-0000-000000000001", "Pending", MediaKind::Movie);
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
    let media = media(&store, "00000000-0000-0000-0000-000000000002", "Frozen", MediaKind::Movie);
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
                probe_required: false,
            }],
            1_000,
        )
        .expect_err("a frozen library must reject publication");
    assert!(matches!(error, store::StoreError::Protected(_)), "{error:?}");
}

/// A path outside the declared root is rejected instead of being written.
#[test]
fn publication_rejects_path_outside_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = media(&store, "00000000-0000-0000-0000-000000000003", "Outside", MediaKind::Movie);
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
    let media = media(&store, "00000000-0000-0000-0000-000000000004", "Nested", MediaKind::Movie);
    let (base, _) = library(&store, "Base", MediaKind::Movie, "/media");
    let (nested, _) = library(&store, "Nested", MediaKind::Movie, "/media/Show");

    let file_id = LedgerId::new();
    store
        .insert_scanned_ledger(
            &LedgerRow {
                id: file_id,
                media_id: media.id,
                path: "/media/Showcase/movie.mkv".into(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            },
            1_000,
        )
        .unwrap();

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
    let source = media(&store, "00000000-0000-0000-0000-000000000005", "Source", MediaKind::Movie);
    let target = media(&store, "00000000-0000-0000-0000-000000000006", "Target", MediaKind::Movie);
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
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();

    store.rebind_scanned_ledger(file.file_id, target.id).unwrap();

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
    let media = media(&store, "00000000-0000-0000-0000-000000000007", "Stable", MediaKind::Movie);
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
        probe_required: false,
    };
    store.publish_files(&scope, &[publication()], 1_000).unwrap();
    let report = store
        .publish_files(&scope, &[publication()], 2_000)
        .expect("republishing identical facts must succeed");
    assert_eq!(report.unchanged, 1);
    assert_eq!(report.updated, 0);
    assert!(store.get_file_details(file.file_id).unwrap().is_none());
}
