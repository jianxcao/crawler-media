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
        .find(|candidate| candidate.library_id.as_deref() == Some(library.id.as_str()))
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
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    file
}

fn observation(row: domain::LedgerRow, missing_at: Option<i64>, presence: FilePresence) -> LedgerPresenceObservation {
    LedgerPresenceObservation {
        row,
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
        .ledger_by_path(&path)
        .unwrap()
        .expect("pending file row");
    assert!(store.transfer_pending(&path).unwrap());

    for mode in [LedgerPresenceMode::Verify, LedgerPresenceMode::Prune] {
        let result = store
            .apply_ledger_presence(
                &[observation(row.clone(), None, ObservedPresence::Missing)],
                mode,
                100,
            )
            .unwrap();
        assert_eq!(result.skipped, 1);
        assert!(store.ledger_by_path(&path).unwrap().is_some());
        assert!(store.missing_at_by_path().unwrap().is_empty());
    }

    let result = store
        .apply_ledger_presence(
            &[observation(row.clone(), None, ObservedPresence::Present)],
            LedgerPresenceMode::Verify,
            200,
        )
        .unwrap();
    assert_eq!(result.restored, 0);
    assert!(!store.transfer_pending(&path).unwrap());
    assert_eq!(file.file_id, row.id);
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
    let old_row = store.ledger_by_path(&path).unwrap().unwrap();

    // The file disappears and a replacement lands on the same path.
    store.delete_ledger_path(&path).unwrap();
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
    let stale = observation(old_row, Some(1000), ObservedPresence::Missing);
    let result = store
        .apply_ledger_presence(&[stale], LedgerPresenceMode::Prune, 1050)
        .unwrap();
    assert_eq!(result.deleted, 0);
    assert_eq!(result.skipped, 1);
    assert!(
        store
            .get_ledger(&replacement.file_id.to_string())
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
    let row = store.ledger_by_path(&path).unwrap().unwrap();

    store
        .apply_ledger_presence(
            &[observation(row.clone(), None, ObservedPresence::Missing)],
            LedgerPresenceMode::Verify,
            1000,
        )
        .unwrap();
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
        Some(1000)
    );

    let result = store
        .apply_ledger_presence(
            &[observation(row.clone(), Some(1000), ObservedPresence::Present)],
            LedgerPresenceMode::Verify,
            2000,
        )
        .unwrap();
    assert_eq!(result.restored, 1);
    assert_eq!(result.missing, 0);
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
        None
    );
    let fetched = store.get_ledger(&file.file_id.to_string()).unwrap().unwrap();
    assert_eq!(fetched.id, file.file_id);
    assert_eq!(fetched.path, row.path);
    assert_eq!(fetched.media_id, row.media_id);
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
    let row = store.ledger_by_path(&path).unwrap().unwrap();

    store
        .apply_ledger_presence(
            &[observation(row.clone(), None, ObservedPresence::Missing)],
            LedgerPresenceMode::Verify,
            1000,
        )
        .unwrap();

    // A permission/IO error is reported as Unknown and must not change state.
    let result = store
        .apply_ledger_presence(
            &[observation(row.clone(), Some(1000), ObservedPresence::Unknown)],
            LedgerPresenceMode::Verify,
            2000,
        )
        .unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(result.missing, 0);
    assert_eq!(result.restored, 0);
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
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
    let first_row = store.ledger_by_path(&first_path).unwrap().unwrap();
    let second_row = store.ledger_by_path(&second_path).unwrap().unwrap();

    let mark_missing = vec![
        observation(first_row.clone(), None, ObservedPresence::Missing),
        observation(second_row.clone(), None, ObservedPresence::Missing),
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
        observation(first_row.clone(), Some(1000), ObservedPresence::Missing),
        observation(second_row.clone(), Some(1000), ObservedPresence::Missing),
    ];
    let result = store.apply_ledger_presence(&prune, LedgerPresenceMode::Prune, 2000);
    assert!(result.is_err(), "batch prune must fail due to trigger");

    assert!(store.get_ledger(&first.file_id.to_string()).unwrap().is_some());
    assert!(store.get_ledger(&second.file_id.to_string()).unwrap().is_some());
}
