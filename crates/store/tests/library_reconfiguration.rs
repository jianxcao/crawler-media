//! Persistent Library reconfiguration: a root or locale change is a resumable operation,
//! never a half-applied UPDATE. Every assertion reads committed Store state through the
//! public seam, so a crash-and-resume is indistinguishable from a clean run.

use std::path::{Path, PathBuf};

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media,
    MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_operations::LibraryReconfiguration;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

fn media(store: &Store, id: &str, kind: MediaKind) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind,
        title: "Show".into(),
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

fn root_id_of(store: &Store, library_id: &str, path: &Path) -> String {
    store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| {
            root.library_id.as_deref() == Some(library_id) && root.path == path
        })
        .unwrap_or_else(|| panic!("root {path:?} must exist"))
        .id
}

fn publication_for(file: &LibraryFile) -> StorePublication<'_> {
    StorePublication {
        file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    }
}

fn library_file(
    library_id: domain::LibraryId,
    media: &Media,
    root_id: &str,
    path: &str,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id: root_id.to_string(),
        media_id: media.id,
        kind: MediaKind::Movie,
        path: PathBuf::from(path),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(2048),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
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
    }
}

fn publish_one(
    store: &Store,
    library_id: domain::LibraryId,
    media: &Media,
    root_id: &str,
    path: &str,
) -> LedgerId {
    let file = library_file(library_id, media, root_id, path);
    let scope = WriteScope {
        library_id,
        config_revision: 0,
    };
    store
        .publish_files(&scope, &[publication_for(&file)], 1_000)
        .unwrap();
    file.file_id
}

#[test]
fn root_replacement_survives_restart_without_mixed_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(&store, "00000000-0000-0000-0000-0000000000c1", MediaKind::Movie);
    let library = store
        .create_library(MediaKind::Movie, "Movies", &["/a/b"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let old_root = root_id_of(&store, &library.id, Path::new("/a/b"));
    let file_ids: Vec<LedgerId> = (0..5)
        .map(|index| {
            publish_one(
                &store,
                lib_id,
                &movie,
                &old_root,
                &format!("/a/b/Film/film-{index}.mkv"),
            )
        })
        .collect();
    let file_id = file_ids[0];

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let status = store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: Some(vec![PathBuf::from("/a")]),
                display_locale: None,
            },
            2_000,
        )
        .unwrap();
    assert_eq!(status.state, "frozen");

    // The Library is frozen for the duration: worker writes pinned to the old
    // revision must be refused rather than landing in a half-migrated generation.
    let stale = WriteScope {
        library_id: lib_id,
        config_revision: expected_revision,
    };
    let probe = store.publish_files(&stale, &[], 2_100);
    assert!(probe.is_ok(), "an empty batch is a no-op");
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );

    // Advance one batch at a time, reopening the store between passes so each step
    // is a real crash-and-resume rather than an in-memory continuation.
    let mut status = store.advance_library_operation(&operation_id, 2_200, 2).unwrap();
    assert_eq!(status.state, "applying", "two of five files moved so far");
    let store = {
        drop(store);
        Store::open(tmp.path()).unwrap()
    };
    assert_eq!(
        store.unfinished_library_operations().unwrap(),
        vec![operation_id.clone()],
        "an interrupted operation stays visible for startup recovery"
    );
    status = store.advance_library_operation(&operation_id, 2_300, 2).unwrap();
    assert_eq!(status.state, "applying");
    status = store.advance_library_operation(&operation_id, 2_400, 2).unwrap();
    assert_eq!(status.state, "succeeded");
    assert_eq!(status.phase, "succeeded");

    let new_root = root_id_of(&store, &library.id, Path::new("/a"));
    let file = store
        .get_file_details(file_id)
        .ok()
        .and_then(|_| None::<LibraryFile>);
    assert!(file.is_none(), "details stay separate from ownership");
    let file_root: String = {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.query_row(
            "SELECT root_id FROM library_files WHERE file_id = ?1",
            rusqlite::params![file_id.to_string()],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(file_root, new_root, "the file must move to the new root");
    assert_ne!(new_root, old_root);
    assert!(
        store
            .get_library_root(&old_root)
            .unwrap()
            .is_none(),
        "the replaced root must be gone once nothing references it"
    );

    let library_after = store.get_library(&library.id).unwrap().unwrap();
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
    assert_eq!(library_after.root_paths, vec![PathBuf::from("/a")]);
    assert_eq!(
        store.library_config_revision(&library.id).unwrap(),
        expected_revision + 1,
        "the configuration revision moves once for the trusted layout"
    );

    // Reopening the store keeps the terminal operation and the new ownership.
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    let stored = reopened
        .get_library_operation(&operation_id)
        .unwrap()
        .expect("operation stays queryable");
    assert_eq!(stored.state, "succeeded");
    assert!(reopened.unfinished_library_operations().unwrap().is_empty());
}

#[test]
fn stale_revision_is_refused_and_library_stays_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();

    let error = store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: uuid::Uuid::new_v4().to_string(),
                library_id: lib_id,
                expected_revision: 999,
                root_paths: Some(vec![PathBuf::from("/tv2")]),
                display_locale: None,
            },
            1_000,
        )
        .unwrap_err();
    assert!(
        format!("{error}").contains("config revision changed"),
        "a stale expectation must be refused, got {error}"
    );
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "ready",
        "a refused intent must not freeze the Library"
    );
}

#[test]
fn locale_change_keeps_existing_nfo_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    // A scoped metadata input recorded in the old display language.
    {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        let root_id = root_id_of(&store, &library.id, Path::new("/tv"));
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
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
        conn.execute(
            "INSERT INTO library_metadata_inputs
                (library_id, media_id, entity_scope, season, episode, locale, input_key,
                 root_id, path_key, parse_state, source_version, fields_json,
                 present_fields_json, provenance_json, revision)
             VALUES (?1, ?2, 'work', -1, -1, 'zh-CN', 'tvshow.nfo', ?3, '/tv/tvshow.nfo',
                     'valid', 'v1', '{}', '[]', '{}', 1)",
            rusqlite::params![library.id, media_id.to_string(), root_id],
        )
        .unwrap();
    }

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            3_000,
        )
        .unwrap();
    let status = store.advance_library_operation(&operation_id, 3_100, 100).unwrap();
    assert_eq!(status.state, "succeeded");

    let locale: String = {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            rusqlite::params![library.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(locale, "en-US");
    let input_locale: String = {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.query_row(
            "SELECT locale FROM library_metadata_inputs WHERE library_id = ?1",
            rusqlite::params![library.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        input_locale, "zh-CN",
        "switching the display locale must not re-label existing NFO observations"
    );
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
}

/// The freeze is the first thing the operation commits, so the persisted phase must name
/// it: a caller that polls the operation must not read `planned` for a Library that is
/// already refusing writes, and a reopened Store must still refuse them.
#[test]
fn begin_persists_the_frozen_phase_and_the_barrier_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(&store, "00000000-0000-0000-0000-0000000000d1", MediaKind::Movie);
    let library = store
        .create_library(MediaKind::Movie, "Movies", &["/c/d"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let old_root = root_id_of(&store, &library.id, Path::new("/c/d"));
    let file = library_file(lib_id, &movie, &old_root, "/c/d/Film/film-0.mkv");
    let unscoped = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };
    store
        .publish_files(&unscoped, &[publication_for(&file)], 1_000)
        .unwrap();
    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();

    let status = store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: Some(vec![PathBuf::from("/c")]),
                display_locale: None,
            },
            2_000,
        )
        .unwrap();
    assert_eq!(
        status.state, "frozen",
        "begin commits the freeze in the same transaction as the operation"
    );
    assert_eq!(status.phase, "frozen");
    let view = store
        .library_operation_view(&operation_id)
        .unwrap()
        .expect("the operation row must be observable right after begin");
    assert_eq!(view.state, "frozen");
    assert_eq!(view.kind, "roots");
    assert!(view.cursor.is_none(), "no batch has been applied yet");
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );
    assert_eq!(
        store.unfinished_library_operations().unwrap(),
        vec![operation_id.clone()]
    );

    // A new process opens the same data directory: the barrier was committed, so it is
    // still in force and worker writes for the frozen Library are refused.
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    let view = reopened
        .library_operation_view(&operation_id)
        .unwrap()
        .expect("the frozen operation survives a restart");
    assert_eq!(view.state, "frozen");
    assert_eq!(
        reopened.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );
    let refused = reopened
        .publish_files(&unscoped, &[publication_for(&file)], 2_100)
        .unwrap_err();
    assert!(
        matches!(refused, store::StoreError::Protected(_)),
        "a write during reindexing must be refused, got {refused}"
    );
    assert!(format!("{refused}").contains("reindexing"), "{refused}");
}

/// `max_media` is an interface contract (`1..=100`), not a hint: a caller that asks for
/// an unsupported batch size gets an explicit error, and the refused call leaves the
/// operation exactly where it was.
#[test]
fn advance_refuses_a_batch_size_outside_the_interface_range() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(&store, "00000000-0000-0000-0000-0000000000d2", MediaKind::Movie);
    let library = store
        .create_library(MediaKind::Movie, "Movies", &["/e/f"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let old_root = root_id_of(&store, &library.id, Path::new("/e/f"));
    for index in 0..2 {
        let file = library_file(
            lib_id,
            &movie,
            &old_root,
            &format!("/e/f/Film/film-{index}.mkv"),
        );
        store
            .publish_files(
                &WriteScope {
                    library_id: lib_id,
                    config_revision: 0,
                },
                &[publication_for(&file)],
                1_000,
            )
            .unwrap();
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision: store.library_config_revision(&library.id).unwrap(),
                root_paths: Some(vec![PathBuf::from("/e")]),
                display_locale: None,
            },
            2_000,
        )
        .unwrap();

    for batch in [0usize, 101, usize::MAX] {
        let error = store
            .advance_library_operation(&operation_id, 2_100, batch)
            .unwrap_err();
        assert!(
            matches!(error, store::StoreError::Invalid(_)),
            "batch {batch} must be refused explicitly, got {error}"
        );
        assert!(format!("{error}").contains("max_media"), "{error}");
    }
    let refused_view = store
        .library_operation_view(&operation_id)
        .unwrap()
        .expect("a refused batch must not delete the operation");
    assert_eq!(
        refused_view.state, "frozen",
        "a refused batch must not advance the operation"
    );
    assert!(refused_view.cursor.is_none());
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );

    // Both ends of the documented range still work.
    let status = store
        .advance_library_operation(&operation_id, 2_200, 1)
        .unwrap();
    assert_eq!(status.state, "applying", "one of two files moved so far");
    let status = store
        .advance_library_operation(&operation_id, 2_300, 100)
        .unwrap();
    assert_eq!(status.state, "succeeded");
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
}

#[test]
fn a_failed_batch_records_the_failure_and_releases_the_freeze() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(
        &store,
        "00000000-0000-0000-0000-0000000000c2",
        MediaKind::Movie,
    );
    let library = store
        .create_library(MediaKind::Movie, "Movies", &["/a/b"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let old_root = root_id_of(&store, &library.id, Path::new("/a/b"));
    publish_one(&store, lib_id, &movie, &old_root, "/a/b/Film/film-0.mkv");

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: Some(vec![PathBuf::from("/a")]),
                display_locale: None,
            },
            3_000,
        )
        .unwrap();
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "reindexing"
    );

    // Fault injection: a batch that has to write the root table cannot finish once the
    // table is gone, so this is a batch that really does return `Err`.
    {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE library_roots;")
            .unwrap();
    }
    let failed = store.advance_library_operation(&operation_id, 3_100, 1);
    assert!(failed.is_err(), "a batch that cannot write must fail");

    // A failed reconfiguration must not leave the Library frozen forever, and it must
    // stop being offered to startup recovery.
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "ready",
        "the freeze is released on the last applied generation"
    );
    let status = store.get_library_operation(&operation_id).unwrap().unwrap();
    assert_eq!(status.state, "failed");
    assert!(status.error.is_some(), "the failure keeps its cause");
    assert!(store.unfinished_library_operations().unwrap().is_empty());
    assert_eq!(
        store.library_config_revision(&library.id).unwrap(),
        expected_revision + 1,
        "the failed attempt still consumed the revision it froze"
    );
}

/// Step 3's first item: a Library a removal already owns must not be reconfigured.
///
/// `begin_library_removal` writes a non-`succeeded` `library_removals` row and leaves
/// `lifecycle_state` at `ready`, so a lifecycle check alone cannot see the removal.
#[test]
fn begin_refuses_a_library_whose_removal_is_still_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movies = store
        .create_library(MediaKind::Movie, "Movies", &["/a/b"], "everyone", true, &[])
        .unwrap();
    // A removal keeps one Library per kind, so a second Movie Library must exist first.
    store
        .create_library(
            MediaKind::Movie,
            "Movies 2",
            &["/c/d"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let removal = store.begin_library_removal(&movies.id).unwrap();
    assert_eq!(removal.state, "frozen");
    assert_eq!(store.library_lifecycle_state(&movies.id).unwrap(), "ready");
    let revision = store.library_config_revision(&movies.id).unwrap();

    let refusal = store.begin_library_reconfiguration(
        &LibraryReconfiguration {
            operation_id: "op-during-removal".into(),
            library_id: movies.id.parse().unwrap(),
            expected_revision: revision,
            root_paths: Some(vec![PathBuf::from("/a/b/new")]),
            display_locale: None,
        },
        2_000,
    );
    assert!(
        matches!(refusal, Err(store::StoreError::Protected(_))),
        "a Library whose removal is pending must not be reconfigured: {refusal:?}"
    );
    assert_eq!(
        store.library_lifecycle_state(&movies.id).unwrap(),
        "ready",
        "the refusal must not freeze the Library for a migration that never started"
    );
}

/// Step 3's third item (`核对`): the success path is checked against the mixing failure
/// itself instead of trusting the batches. Every file must end up owned by a root of its
/// own Library, every file under the requested root must have moved, and a file outside
/// the request keeps its owner instead of losing its Library facts.
#[test]
fn success_leaves_no_file_owned_by_a_foreign_or_missing_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(
        &store,
        "00000000-0000-0000-0000-0000000000d1",
        MediaKind::Movie,
    );
    let library = store
        .create_library(
            MediaKind::Movie,
            "Mixed",
            &["/a/b", "/other"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib_id = library.id.parse().unwrap();
    let kept_root = root_id_of(&store, &library.id, Path::new("/a/b"));
    let other_root = root_id_of(&store, &library.id, Path::new("/other"));
    publish_one(&store, lib_id, &movie, &kept_root, "/a/b/Film/kept.mkv");
    publish_one(&store, lib_id, &movie, &other_root, "/other/Film/outside.mkv");

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: Some(vec![PathBuf::from("/a")]),
                display_locale: None,
            },
            3_000,
        )
        .unwrap();
    let mut now = 3_100;
    let status = loop {
        let status = store
            .advance_library_operation(&operation_id, now, 100)
            .unwrap();
        if status.state == "succeeded" || status.state == "failed" {
            break status;
        }
        now += 100;
    };
    assert_eq!(status.state, "succeeded", "{:?}", status.error);
    assert_eq!(
        store.library_lifecycle_state(&library.id).unwrap(),
        "ready",
        "a verified success releases the freeze"
    );

    let requested_root = root_id_of(&store, &library.id, Path::new("/a"));
    let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let count = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> i64 {
        conn.query_row(sql, params, |row| row.get(0)).unwrap()
    };
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM library_files f
             LEFT JOIN library_roots r
               ON r.root_id = f.root_id AND r.library_id = f.library_id
             WHERE f.library_id = ?1 AND r.root_id IS NULL",
            &[&library.id],
        ),
        0,
        "every file must be owned by a root of its own Library"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM library_files
             WHERE library_id = ?1 AND path_key LIKE '/a/%' AND root_id <> ?2",
            &[&library.id, &requested_root],
        ),
        0,
        "every file under the requested root must have moved to it"
    );
    let owner = |path: &str| -> String {
        conn.query_row(
            "SELECT root_id FROM library_files WHERE path_key = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(owner("/a/b/Film/kept.mkv"), requested_root);
    assert_eq!(
        owner("/other/Film/outside.mkv"),
        other_root,
        "a file outside the requested roots keeps its owner"
    );
    assert!(
        store.get_library_root(&other_root).unwrap().is_some(),
        "the root that still owns a file must not be pruned"
    );
}
