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
        .enqueue_file_deletion("/nowhere/file.mkv", "wash_replaced")
        .unwrap();
    for _ in 0..store::MAX_FILE_DELETION_ATTEMPTS {
        store
            .record_file_deletion_failure("/nowhere/file.mkv", 0, "EACCES")
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

    store.enqueue_file_deletion(&path, "wash_replaced").unwrap();

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
    store.complete_file_deletion(&path).unwrap();
    assert!(
        store
            .file_coverage_by_id(row.file_id.expect("fixture file id"))
            .unwrap()
            .is_none()
    );
    assert!(store.list_pending_file_deletions().unwrap().is_empty());
}
