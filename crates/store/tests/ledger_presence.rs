//! Presence writes: verification marks/clears `missing_at`, pruning removes already
//! missing files, and a stale observation never touches a replacement file.

use std::path::Path;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::{
    FilePresence as ObservedPresence, LedgerPresenceMode, LedgerPresenceObservation, Store,
};

fn sample_media(id: MediaId) -> Media {
    Media {
        id,
        kind: MediaKind::Movie,
        title: "Test Movie".to_string(),
        original_title: None,
        aliases: Vec::new(),
        year: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn create_library(store: &Store, root: &Path) -> (store::Library, String) {
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_str() == library.id.as_str())
        .unwrap()
        .id;
    (library, root_id)
}

fn publish_file(
    store: &Store,
    library: &store::Library,
    root_id: &str,
    media: MediaId,
    path: &str,
    publication: FilePublication,
    file_id: LedgerId,
) -> LibraryFile {
    let file = LibraryFile {
        file_id,
        library_id: library.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id: media,
        kind: MediaKind::Movie,
        path: path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: domain::FilePresence::Present,
        publication,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(10),
        modified_ns: Some(20),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    store
        .publish_files(
            &WriteScope {
                library_id: library.id.parse().unwrap(),
                config_revision: 0,
            },
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
    file
}

#[test]
fn tv_library_removal_recovers_empty_coverage_and_preserves_file_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let root = temp.path().join("tv");
    let other_root = temp.path().join("other-tv");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other_root).unwrap();
    let store = Store::open(&data).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Other TV",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Uncovered episode".into(),
        original_title: None,
        aliases: Vec::new(),
        year: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("uncovered.mkv");
    let bytes = b"video facts must survive library deletion";
    let file = publish_uncovered_tv_file(&store, &library, media.id, &path, bytes);
    assert!(store.get_file_units(file.file_id).unwrap().is_empty());

    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    drop(store);

    let store = Store::open(&data).unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 1);
    assert!(store.get_library(&library.id).unwrap().is_none());
    assert!(store.file_coverage_by_id(file.file_id).unwrap().is_none());
    assert!(store.get_media(media.id).unwrap().is_some());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(
        store.clean_library_removal(&removal.operation_id).unwrap(),
        done
    );
}

fn publish_uncovered_tv_file(
    store: &Store,
    library: &store::Library,
    media_id: MediaId,
    path: &Path,
    bytes: &[u8],
) -> LibraryFile {
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_str() == library.id.as_str())
        .unwrap()
        .id;
    std::fs::write(path, bytes).unwrap();
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id,
        media_id,
        kind: MediaKind::Tv,
        path: path.to_path_buf(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(bytes.len() as u64),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    store
        .publish_files(
            &WriteScope {
                library_id: library.id.parse().unwrap(),
                config_revision: 0,
            },
            &[StorePublication {
                file: &file,
                units: &[],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();
    file
}

fn observation(
    store: &Store,
    row: domain::FileCoverage,
    missing_at: Option<i64>,
    presence: FilePresence,
) -> LedgerPresenceObservation {
    let path_key = if row.path_key.is_empty() {
        store.recorded_path_key(&row.file.path.to_string_lossy())
    } else {
        row.path_key.clone()
    };
    LedgerPresenceObservation {
        file: row,
        path_key,
        missing_at,
        presence,
    }
}

#[test]
fn unpublished_transfer_survives_presence_and_present_publication_publishes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let path = tmp.path().join("pending.mkv").display().to_string();
    let file = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &path,
        FilePublication::Pending,
        LedgerId::new(),
    );
    let row = store
        .file_coverage_by_path(&path)
        .unwrap()
        .expect("pending file row");
    assert!(store.transfer_pending(&path).unwrap());

    for mode in [LedgerPresenceMode::Verify, LedgerPresenceMode::Prune] {
        let result = store
            .apply_ledger_presence(
                &[observation(
                    &store,
                    row.clone(),
                    None,
                    ObservedPresence::Missing,
                )],
                mode,
                100,
            )
            .unwrap();
        assert_eq!(result.skipped, 1);
        assert!(store.file_coverage_by_path(&path).unwrap().is_some());
        assert!(store.missing_at_by_path().unwrap().is_empty());
    }

    let result = store
        .apply_ledger_presence(
            &[observation(
                &store,
                row.clone(),
                None,
                ObservedPresence::Present,
            )],
            LedgerPresenceMode::Verify,
            200,
        )
        .unwrap();
    assert_eq!(result.restored, 0);
    assert!(!store.transfer_pending(&path).unwrap());
    assert_eq!(file.file_id, row.file.file_id);
    assert!(
        store
            .get_library_item(library.id.parse().unwrap(), media_id)
            .unwrap()
            .is_some(),
        "a recovered publication becomes library content"
    );
}

#[test]
fn stale_observation_does_not_delete_replacement_file() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let path = tmp.path().join("one.mkv").display().to_string();
    let old = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &path,
        FilePublication::Published,
        LedgerId::new(),
    );
    let old_row = store.file_coverage_by_path(&path).unwrap().unwrap();

    // The file disappears and a replacement lands on the same path.
    store.delete_ledger_path(&path).unwrap();
    // The last file's delete purged the orphan Media; the replacement re-identifies it.
    store.insert_media(&sample_media(media_id)).unwrap();
    let replacement = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &path,
        FilePublication::Published,
        LedgerId::new(),
    );
    assert_ne!(replacement.file_id, old.file_id);

    // A stale prune observation still holding the old identity.
    let stale = observation(&store, old_row, Some(1000), ObservedPresence::Missing);
    let result = store
        .apply_ledger_presence(&[stale], LedgerPresenceMode::Prune, 1050)
        .unwrap();
    assert_eq!(result.deleted, 0);
    assert_eq!(result.skipped, 1);
    assert!(
        store
            .file_coverage_by_id(replacement.file_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn restored_file_clears_flag_without_replacing_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let path = tmp.path().join("two.mkv").display().to_string();
    let file = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &path,
        FilePublication::Published,
        LedgerId::new(),
    );
    let row = store.file_coverage_by_path(&path).unwrap().unwrap();

    store
        .apply_ledger_presence(
            &[observation(
                &store,
                row.clone(),
                None,
                ObservedPresence::Missing,
            )],
            LedgerPresenceMode::Verify,
            1000,
        )
        .unwrap();
    assert_eq!(
        store
            .missing_at_by_path()
            .unwrap()
            .get(&row.path_key)
            .copied(),
        Some(1000)
    );

    let result = store
        .apply_ledger_presence(
            &[observation(
                &store,
                row.clone(),
                Some(1000),
                ObservedPresence::Present,
            )],
            LedgerPresenceMode::Verify,
            2000,
        )
        .unwrap();
    assert_eq!(result.restored, 1);
    assert_eq!(result.missing, 0);
    assert_eq!(
        store
            .missing_at_by_path()
            .unwrap()
            .get(&row.path_key)
            .copied(),
        None
    );
    let fetched = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_eq!(fetched.file.file_id, file.file_id);
    assert_eq!(fetched.file.path, row.file.path);
    assert_eq!(fetched.file.media_id, row.file.media_id);
}

#[test]
fn unknown_observation_preserves_prior_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let path = tmp.path().join("three.mkv").display().to_string();
    publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &path,
        FilePublication::Published,
        LedgerId::new(),
    );
    let row = store.file_coverage_by_path(&path).unwrap().unwrap();

    store
        .apply_ledger_presence(
            &[observation(
                &store,
                row.clone(),
                None,
                ObservedPresence::Missing,
            )],
            LedgerPresenceMode::Verify,
            1000,
        )
        .unwrap();

    // A permission/IO error is reported as Unknown and must not change state.
    let result = store
        .apply_ledger_presence(
            &[observation(
                &store,
                row.clone(),
                Some(1000),
                ObservedPresence::Unknown,
            )],
            LedgerPresenceMode::Verify,
            2000,
        )
        .unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(result.missing, 0);
    assert_eq!(result.restored, 0);
    assert_eq!(
        store
            .missing_at_by_path()
            .unwrap()
            .get(&row.path_key)
            .copied(),
        Some(1000)
    );
}

#[test]
fn prune_rolls_back_whole_batch_on_derived_error() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let first_path = tmp.path().join("p1.mkv").display().to_string();
    let second_path = tmp.path().join("p2.mkv").display().to_string();
    let first = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &first_path,
        FilePublication::Published,
        LedgerId::new(),
    );
    let second = publish_file(
        &store,
        &library,
        &root_id,
        media_id,
        &second_path,
        FilePublication::Published,
        LedgerId::new(),
    );
    let first_row = store.file_coverage_by_path(&first_path).unwrap().unwrap();
    let second_row = store.file_coverage_by_path(&second_path).unwrap().unwrap();

    let mark_missing = vec![
        observation(&store, first_row.clone(), None, ObservedPresence::Missing),
        observation(&store, second_row.clone(), None, ObservedPresence::Missing),
    ];
    store
        .apply_ledger_presence(&mark_missing, LedgerPresenceMode::Verify, 1000)
        .unwrap();

    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    // SQLite forbids bound parameters inside a trigger body, so the fixture path
    // is inlined as an SQL string literal.
    let second_path_literal = second_path.replace('\'', "''");
    lib_conn
        .execute_batch(&format!(
            "CREATE TRIGGER fail_row2_del BEFORE DELETE ON library_files
             FOR EACH ROW WHEN OLD.display_path = '{second_path_literal}'
             BEGIN
                 SELECT RAISE(ABORT, 'forced trigger abort on row2');
             END;"
        ))
        .unwrap();

    let prune = vec![
        observation(
            &store,
            first_row.clone(),
            Some(1000),
            ObservedPresence::Missing,
        ),
        observation(
            &store,
            second_row.clone(),
            Some(1000),
            ObservedPresence::Missing,
        ),
    ];
    let result = store.apply_ledger_presence(&prune, LedgerPresenceMode::Prune, 2000);
    assert!(result.is_err(), "batch prune must fail due to trigger");

    assert!(store.file_coverage_by_id(first.file_id).unwrap().is_some());
    assert!(store.file_coverage_by_id(second.file_id).unwrap().is_some());
}

#[test]
fn old_generation_never_marks_or_prunes_republished_file() {
    for mode in [LedgerPresenceMode::Verify, LedgerPresenceMode::Prune] {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let media_id = MediaId::new();
        store.insert_media(&sample_media(media_id)).unwrap();
        let (library, root_id) = create_library(&store, tmp.path());
        let path = tmp.path().join("generation.mkv").display().to_string();
        let file = publish_file(
            &store,
            &library,
            &root_id,
            media_id,
            &path,
            FilePublication::Published,
            LedgerId::new(),
        );
        let old = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        if mode == LedgerPresenceMode::Prune {
            store
                .apply_ledger_presence(
                    &[observation(
                        &store,
                        old.clone(),
                        None,
                        ObservedPresence::Missing,
                    )],
                    LedgerPresenceMode::Verify,
                    1000,
                )
                .unwrap();
        }
        let mut replacement = old.file.clone();
        replacement.version.source_version = "v2".into();
        store
            .publish_files(
                &WriteScope {
                    library_id: replacement.library_id,
                    config_revision: 0,
                },
                &[StorePublication {
                    file: &replacement,
                    units: &[UnitKey::WHOLE],
                    expected_version: Some(&old.file.version),
                    nfo_observation: None,
                    probe_required: false,
                }],
                2000,
            )
            .unwrap();
        let result = store
            .apply_ledger_presence(
                &[observation(
                    &store,
                    old,
                    if mode == LedgerPresenceMode::Prune {
                        Some(1000)
                    } else {
                        None
                    },
                    ObservedPresence::Missing,
                )],
                mode,
                3000,
            )
            .unwrap();
        assert_eq!(result.skipped, 1);
        assert!(result.applied.is_empty());
        let current = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        assert_eq!(current.file.version.source_version, "v2");
        assert_eq!(current.file.presence, ObservedPresence::Present);
        assert!(store.missing_at_by_path().unwrap().is_empty());
    }
}
