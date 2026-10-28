use std::sync::Arc;
use std::time::Duration;

use domain::{Media, MediaId, MediaKind};
use parking_lot::Mutex;

use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    file: domain::LibraryFile,
}

impl Fixture {
    fn new() -> Self {
        Self::with_alias(false)
    }

    fn with_alias(alias: bool) -> Self {
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
        let path = root.join("file.mkv");
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
            store: Arc::new(Mutex::new(store)),
            file,
        }
    }

    fn queue(&self) -> store::QueuedFileDeletion {
        let path = self.file.path.to_string_lossy();
        let identity = store::file_identity(&path);
        let store = self.store.lock();
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
    let store = f.store.lock();
    assert!(store.file_coverage_by_id(f.file.file_id).unwrap().is_some());
    assert!(store.list_pending_file_deletions().unwrap().is_empty());
}

#[test]
fn path_appearing_after_an_absent_observation_is_preserved_without_a_file_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Mutex::new(Store::open(dir.path().join("data")).unwrap());
    let path = dir.path().join("appeared.mkv");
    store
        .lock()
        .enqueue_file_deletion_observed(path.to_str().unwrap(), "test", None)
        .unwrap();
    std::fs::write(&path, b"new bytes").unwrap();
    retry_queued_deletions(&store).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"new bytes");
    assert!(
        store
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn committed_deletion_intent_survives_a_fact_cleanup_failure() {
    let f = Fixture::new();
    f.queue();
    let db = rusqlite::Connection::open(f.store.lock().data_dir().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_cleanup BEFORE DELETE ON library_files
        BEGIN SELECT RAISE(ABORT,'cleanup unavailable'); END;",
    )
    .unwrap();
    assert!(retry_queued_deletions(&f.store).is_err());
    assert!(!f.file.path.exists());
    assert!(
        f.store
            .lock()
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.store.lock().list_pending_file_deletions().unwrap().len(),
        1
    );
    db.execute_batch("DROP TRIGGER fail_cleanup").unwrap();
    retry_queued_deletions(&f.store).unwrap();
    assert!(
        f.store
            .lock()
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_none()
    );
    assert!(
        f.store
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty()
    );
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
    f.store.lock().abandon_file_deletion(&entry).unwrap();
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
            .lock()
            .file_coverage_by_id(f.file.file_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn stale_worker_cannot_drain_a_recreated_intent_with_the_same_file_identity() {
    let f = Fixture::new();
    let old = f.queue();
    f.store.lock().abandon_file_deletion(&old).unwrap();
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
    let store = f.store.lock();
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
fn durable_deletion_survives_retirement_of_its_library_root_alias() {
    let f = Fixture::with_alias(true);
    let entry = f.queue();
    {
        let store = f.store.lock();
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
        assert_eq!(
            store.list_due_file_deletions(0).unwrap(),
            vec![entry.clone()]
        );
    }
    retry_queued_deletions(&f.store).unwrap();
    assert!(
        !f.file.path.exists(),
        "retired root alias hid the durable deletion"
    );
    assert!(
        f.store
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty()
    );
}
