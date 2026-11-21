use std::sync::Arc;
use std::time::Duration;

use domain::{Media, MediaId, MediaKind};

use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    file: domain::LibraryFile,
}

impl Fixture {
    fn new() -> Self {
        Self::with_alias(false)
    }

    fn with_alias(alias: bool) -> Self {
        Self::at_path(alias, "file.mkv")
    }

    fn at_path(alias: bool, relative: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("owned");
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        let root = if alias {
            let path = dir.path().join("alias");
            std::os::unix::fs::symlink(&root, &path).unwrap();
            path
        } else {
            root
        };
        #[cfg(not(unix))]
        assert!(!alias);
        store
            .create_library(
                MediaKind::Movie,
                "Delete",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: "Deletion".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"original").unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            _dir: dir,
            store: Arc::new(store),
            file,
        }
    }

    fn queue(&self) -> store::QueuedFileDeletion {
        let path = self.file.path.to_string_lossy();
        let identity = store::file_identity(&path);
        let store = self.store.as_ref();
        store
            .enqueue_file_deletion_observed(&path, "test", identity)
            .unwrap();
        store
            .due_file_deletion(&path, crate::job_loop::unix_now())
            .unwrap()
            .unwrap()
    }
}

#[test]
fn queued_same_inode_equal_size_replacement_is_preserved() {
    let f = Fixture::new();
    let entry = f.queue();
    std::thread::sleep(Duration::from_millis(2));
    std::fs::write(&f.file.path, b"replaced").unwrap();
    assert_eq!(
        store::file_identity(&entry.path).unwrap(),
        (entry.file_size.unwrap(), entry.file_inode)
    );
    retry_queued_deletions(&f.store).unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"replaced");
    let store = &f.store;
    assert!(store.file_coverage_by_id(f.file.file_id).unwrap().is_some());
    assert!(store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn path_appearing_after_an_absent_observation_is_preserved_without_a_file_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let path = dir.path().join("appeared.mkv");
    store
        .enqueue_file_deletion_observed(path.to_str().unwrap(), "test", None)
        .unwrap();
    std::fs::write(&path, b"new bytes").unwrap();
    retry_queued_deletions(&store).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"new bytes");
    assert!(store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn committed_deletion_intent_survives_a_fact_cleanup_failure() {
    let f = Fixture::new();
    f.queue();
    let db = rusqlite::Connection::open(f.store.data_dir().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_cleanup BEFORE DELETE ON library_files
        BEGIN SELECT RAISE(ABORT,'cleanup unavailable'); END;",
    )
    .unwrap();
    assert!(retry_queued_deletions(&f.store).is_err());
    assert!(!f.file.path.exists());
    assert!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(f.store.list_pending_file_deletions().unwrap().len(), 1);
    db.execute_batch("DROP TRIGGER fail_cleanup").unwrap();
    retry_queued_deletions(&f.store).unwrap();
    assert!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_none()
    );
    assert!(f.store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn same_path_worker_rechecks_intent_after_the_previous_claim_finishes() {
    let f = Fixture::new();
    let entry = f.queue();
    let claim = claims::path_claim(&entry.path_key);
    let held = claim.lock();
    let store = f.store.clone();
    let (entered, started) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        entered.send(()).unwrap();
        let result = retry_queued_deletions(&store);
        finished.send(result).unwrap();
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let premature = done.recv_timeout(Duration::from_millis(100));
    // Model a previous owner completing and a new source appearing before the
    // waiting worker can claim the path. It must reload the durable intent.
    f.store.abandon_file_deletion(&entry).unwrap();
    std::fs::write(&f.file.path, b"new source").unwrap();
    drop(held);
    let completion = done.recv_timeout(Duration::from_secs(3));
    worker.join().unwrap();
    assert!(
        premature.is_err(),
        "a second drain ran through the path claim"
    );
    completion.unwrap().unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"new source");
    assert!(
        f.store
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn stale_worker_cannot_drain_a_recreated_intent_with_the_same_file_identity() {
    let f = Fixture::new();
    let old = f.queue();
    f.store.abandon_file_deletion(&old).unwrap();
    let current = f.queue();
    assert_eq!(
        (old.file_id.as_ref(), old.file_size, old.file_inode),
        (
            current.file_id.as_ref(),
            current.file_size,
            current.file_inode
        )
    );
    assert_ne!(old.deletion_id, current.deletion_id);
    process_queued_deletion(&f.store, &old, crate::job_loop::unix_now()).unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"original");
    let store = &f.store;
    assert!(store.file_coverage_by_id(f.file.file_id).unwrap().is_some());
    assert_eq!(
        store
            .due_file_deletion(&current.path, crate::job_loop::unix_now())
            .unwrap(),
        Some(current)
    );
}

#[cfg(unix)]
#[test]
fn library_removal_preserves_bytes_after_root_alias_retirement() {
    let f = Fixture::with_alias(true);
    let entry = f.queue();
    {
        let store = &f.store;
        let operation = store
            .begin_library_removal(&f.file.library_id.to_string())
            .unwrap();
        while !store
            .capture_library_removal_batch(&operation.operation_id)
            .unwrap()
        {}
        store
            .set_removal_state(&operation.operation_id, "cleaning", None)
            .unwrap();
        store
            .clean_library_removal(&operation.operation_id)
            .unwrap();
        assert!(store.list_due_file_deletions(0).unwrap().is_empty());
    }
    process_queued_deletion(&f.store, &entry, crate::job_loop::unix_now()).unwrap();
    retry_queued_deletions(&f.store).unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"original");
    assert!(f.store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn file_fact_removal_does_not_authorize_a_queued_unlink() {
    let f = Fixture::new();
    f.queue();
    f.store
        .delete_files(f.file.library_id, &[f.file.file_id])
        .unwrap();
    retry_queued_deletions(&f.store).unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"original");
    assert!(f.store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn removal_waits_for_an_admitted_file_deletion_before_finishing() {
    struct NoFetch;
    impl indexer::Fetcher for NoFetch {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("network forbidden".into()))
        }
    }
    let f = Fixture::at_path(false, "nested/file.mkv");
    f.store
        .create_library(
            MediaKind::Tv,
            "Nested TV",
            &[f.file.path.parent().unwrap().to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let entry = f.queue();
    let state = crate::ApiState::new_arc(
        f.store.clone(),
        "secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(
            f._dir.path().join("stage"),
        )),
    )
    .unwrap();
    // Model an already admitted filesystem operation. Removal must close this
    // existing path boundary before it reports completion; it must not hold a DB writer.
    let claim = claims::path_claim(&entry.path_key);
    let held = claim.lock();
    // An admitted delete can finish its SQL before freeze, then continue sidecar
    // cleanup. Its claim must be closed even though no file enters the snapshot.
    f.store
        .delete_files(f.file.library_id, &[f.file.file_id])
        .unwrap();
    f.store.abandon_file_deletion(&entry).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let operation =
            crate::library_lifecycle::start_library_removal(&state, &f.file.library_id.to_string())
                .unwrap();
        let complete = || async {
            loop {
                let operation = f
                    .store
                    .library_removal(&operation.operation_id)
                    .unwrap()
                    .unwrap();
                assert_ne!(operation.state, "failed", "{:?}", operation.error);
                if operation.state == "succeeded" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let premature = tokio::time::timeout(Duration::from_millis(100), complete()).await;
        // Reads and unrelated writes remain available while the file boundary is busy.
        f.store.put_setting("removal-test", "available").unwrap();
        drop(held);
        tokio::time::timeout(Duration::from_secs(3), complete())
            .await
            .unwrap();
        assert!(
            premature.is_err(),
            "removal completed before its admitted file operation"
        );
    });
    process_queued_deletion(&f.store, &entry, crate::job_loop::unix_now()).unwrap();
    assert_eq!(std::fs::read(&f.file.path).unwrap(), b"original");
    assert!(f.store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn frozen_library_preserves_queued_sidecars_without_file_rows() {
    let f = Fixture::with_alias(cfg!(unix));
    let path = f.file.path.with_extension("nfo");
    std::fs::write(&path, b"metadata").unwrap();
    let path = path.to_str().unwrap();
    f.store
        .enqueue_file_deletion_observed(
            path,
            "rename-left-behind-sidecar",
            store::file_identity(path),
        )
        .unwrap();
    f.store
        .begin_library_removal(&f.file.library_id.to_string())
        .unwrap();
    retry_queued_deletions(&f.store).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"metadata");
}

#[test]
fn p1_2_consuming_leftover_fanart_keeps_preserved_poster_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let store = Store::open(base.join("data")).unwrap();
    let a = base.join("a");
    let b = base.join("b");
    for (name, root) in [("A", &a), ("B", &b)] {
        std::fs::create_dir_all(root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
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
    let src = a.join("Show");
    let dst = b.join("Show");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    let from = src.join("S01E01.mkv");
    let to = dst.join("S01E01.mkv");
    std::fs::write(&from, b"video").unwrap();
    std::fs::write(src.join("poster.jpg"), b"source-poster").unwrap();
    std::fs::write(src.join("fanart.jpg"), b"same-fanart").unwrap();
    std::fs::write(dst.join("poster.jpg"), b"dest-poster").unwrap();
    std::fs::write(dst.join("fanart.jpg"), b"same-fanart").unwrap();
    let file = store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media,
            path: from.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .record_file_artwork(
            file.file_id,
            &file.version,
            &store::library_write::FileArtwork {
                poster: Some(&src.join("poster.jpg")),
                backdrop: Some(&src.join("fanart.jpg")),
                still: None,
            },
        )
        .unwrap();

    let req = store
        .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
        .unwrap();
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    store.record_rename_disk(&receipt, 2).unwrap();
    store.complete_file_rename(&req.rename_id, 3).unwrap();

    let due = store.list_due_file_deletions(i64::MAX).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].scope, store::FileDeletionScope::ExactPath);

    // Consume queue twice (covers idempotent repeated consumption)
    retry_queued_deletions(&store).unwrap();
    retry_queued_deletions(&store).unwrap();

    assert!(store.list_pending_file_deletions().unwrap().is_empty());
    assert!(!src.join("fanart.jpg").exists(), "old fanart consumed");
    assert_eq!(
        std::fs::read(src.join("poster.jpg")).unwrap(),
        b"source-poster",
        "preserved old poster must NOT be pruned"
    );
    assert_eq!(
        std::fs::read(dst.join("poster.jpg")).unwrap(),
        b"dest-poster",
        "destination poster intact"
    );
    assert_eq!(
        std::fs::read(dst.join("fanart.jpg")).unwrap(),
        b"same-fanart",
        "destination fanart intact"
    );
    let updated = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_eq!(
        updated.file.poster_path.as_deref(),
        Some(src.join("poster.jpg").as_path())
    );
    assert_eq!(
        updated.file.backdrop_path.as_deref(),
        Some(dst.join("fanart.jpg").as_path())
    );
}

#[test]
fn p1_3_consuming_queued_nfo_deletion_preserves_source_if_target_vanishes_or_changes() {
    for mode in ["delete", "replace", "intact"] {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let store = Store::open(base.join("data")).unwrap();
        let a = base.join("a");
        let b = base.join("b");
        for (name, root) in [("A", &a), ("B", &b)] {
            std::fs::create_dir_all(root).unwrap();
            store
                .create_library(
                    MediaKind::Movie,
                    name,
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        }
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: "Dune".into(),
                year: Some(2021),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let src = a.join("Dune");
        let dst = b.join("Dune");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let from = src.join("Dune.mkv");
        let from_nfo = src.join("Dune.nfo");
        let to = dst.join("Dune.mkv");
        let to_nfo = dst.join("Dune.nfo");
        std::fs::write(&from, b"video").unwrap();
        std::fs::write(&from_nfo, b"<movie><title>Dune</title></movie>").unwrap();
        std::fs::write(&to_nfo, b"<movie><title>Dune</title></movie>").unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media,
                path: from.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let req = store
            .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
            .unwrap();
        let receipt = store::library_write::execute_rename_disk(&req).unwrap();
        store.record_rename_disk(&receipt, 2).unwrap();
        store.complete_file_rename(&req.rename_id, 3).unwrap();
        assert_eq!(store.list_pending_file_deletions().unwrap().len(), 1);

        // Mutate target AFTER queueing, BEFORE background deletion worker consumes it
        match mode {
            "delete" => std::fs::remove_file(&to_nfo).unwrap(),
            "replace" => std::fs::write(&to_nfo, b"<movie><title>Other</title></movie>").unwrap(),
            "intact" => {}
            _ => unreachable!(),
        }

        retry_queued_deletions(&store).unwrap();
        assert!(store.list_pending_file_deletions().unwrap().is_empty());
        if mode == "intact" {
            assert!(!from_nfo.exists(), "intact target allows deleting old NFO");
            assert_eq!(
                std::fs::read(&to_nfo).unwrap(),
                b"<movie><title>Dune</title></movie>"
            );
        } else {
            assert_eq!(
                std::fs::read(&from_nfo).unwrap(),
                b"<movie><title>Dune</title></movie>",
                "mode={mode}: old NFO must be preserved when target vanished/changed before unlink"
            );
        }
    }
}
