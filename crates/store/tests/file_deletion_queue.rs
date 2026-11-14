//! Review L-10 / M-3: the durable file-deletion queue.
//!
//! A permanently failing deletion stops being retried after a bounded number of
//! attempts and stays visible with its last error; a successful retry clears both
//! the queue row and the Library file row it named.

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use store::Store;
use store::test_support::FixtureFile;

fn create_media(store: &Store, title: &str) -> Media {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media
}

fn create_library(store: &Store, root: &std::path::Path) -> store::Library {
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
}

#[test]
fn permanently_failing_deletion_stops_retrying() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store
        .enqueue_file_deletion_observed("/nowhere/file.mkv", "wash_replaced", None)
        .unwrap();
    let intent = store.list_due_file_deletions(0).unwrap().pop().unwrap();
    for _ in 0..store::MAX_FILE_DELETION_ATTEMPTS {
        store
            .record_file_deletion_failure(&intent, 0, "EACCES")
            .unwrap();
    }
    assert!(store.list_due_file_deletions(i64::MAX).unwrap().is_empty());
    let stuck = store.list_stuck_file_deletions().unwrap();
    assert_eq!(
        stuck,
        vec![("/nowhere/file.mkv".to_string(), Some("EACCES".to_string()))]
    );
}

#[test]
fn queued_deletion_retries_by_file_id_and_clears_both_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    create_library(&store, tmp.path());
    let media = create_media(&store, "Matrix");
    let path = tmp.path().join("Matrix.mkv");
    std::fs::write(&path, b"bytes").unwrap();
    let path = path.display().to_string();

    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media.id,
        path: path.clone(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(&store, &row).unwrap();

    store
        .enqueue_file_deletion_observed(&path, "wash_replaced", store::file_identity(&path))
        .unwrap();

    // The queue names the file by the current Library file id, not by path alone.
    let due = store.list_due_file_deletions(i64::MAX).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        due[0].file_id.as_deref(),
        Some(row.file_id.expect("fixture file id").to_string().as_str())
    );
    assert_eq!(due[0].file_size, Some(5));
    assert_eq!(
        store.file_id_at_path(&path).unwrap(),
        Some(row.file_id.expect("fixture file id").to_string())
    );

    // Retry completion: the bytes are gone, so the queue row and the file row go.
    std::fs::remove_file(&path).unwrap();
    store.complete_file_deletion(&due[0]).unwrap();
    assert!(
        store
            .file_coverage_by_id(row.file_id.expect("fixture file id"))
            .unwrap()
            .is_none()
    );
    assert!(store.list_pending_file_deletions().unwrap().is_empty());
}
#[test]
fn queued_deletion_keeps_the_identity_observed_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store::Store::open(tmp.path().join("data")).unwrap();
    let path = tmp.path().join("observed.mkv");
    std::fs::write(&path, b"first").unwrap();
    let identity = store::file_identity(path.to_str().unwrap()).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replaced-after-observation").unwrap();
    store
        .enqueue_file_deletion_observed(path.to_str().unwrap(), "test", Some(identity))
        .unwrap();
    let queued = store.list_due_file_deletions(i64::MAX).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].file_size, Some(5));
    assert_eq!(queued[0].file_inode, identity.1);
    assert_eq!(std::fs::read(path).unwrap(), b"replaced-after-observation");
}

#[cfg(unix)]
#[test]
fn durable_deletion_completion_survives_library_alias_retirement() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let real = tmp.path().join("real");
    let alias = tmp.path().join("alias");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let library = create_library(&store, &alias);
    let media = create_media(&store, "Alias");
    let path = alias.join("file.mkv");
    std::fs::write(&path, b"bytes").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .enqueue_file_deletion_observed(
            path.to_str().unwrap(),
            "review",
            store::file_identity(path.to_str().unwrap()),
        )
        .unwrap();
    let entry = store.list_due_file_deletions(0).unwrap().pop().unwrap();
    let operation = store.begin_library_removal(&library.id).unwrap();
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
    for _ in 0..store::MAX_FILE_DELETION_ATTEMPTS {
        store
            .record_file_deletion_failure(&entry, 0, "offline")
            .unwrap();
    }
    assert_eq!(store.list_stuck_file_deletions().unwrap().len(), 1);
    assert!(store.file_deletion_pending(&entry.path).unwrap());
    store.reset_file_deletion_backoff(&entry.path).unwrap();
    assert_eq!(
        store.due_file_deletion(&entry.path, 0).unwrap(),
        Some(entry.clone())
    );
    std::fs::remove_file(&entry.path).unwrap();
    store.complete_file_deletion(&entry).unwrap();
    assert!(
        store.list_pending_file_deletions().unwrap().is_empty(),
        "alias retirement hid the intent from completion"
    );
}
