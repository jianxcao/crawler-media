use std::path::Path;

use domain::{LedgerId, Media, MediaId, MediaKind, UserId};
use rusqlite::params;

use super::*;
use crate::test_support::FixtureFile;
use crate::{Store, StoreError};

fn fixture(store: &Store, root: &Path) -> (crate::Library, FixtureFile) {
    std::fs::create_dir_all(root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Example".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media.id,
        path: root.join("E01.strm").display().to_string(),
        season: Some(1),
        episode: Some(1),
        ..FixtureFile::default()
    };
    crate::test_support::publish_fixture_file(store, &row).unwrap();
    (library, row)
}

#[test]
fn movie_library_removal_accepts_the_whole_media_unit() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
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
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Whole".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media.id,
        path: root.join("Whole.mkv").display().to_string(),
        ..FixtureFile::default()
    };
    // A movie publishes the canonical whole-media unit, stored as (-1, -1).
    crate::test_support::publish_fixture_file(&store, &row).unwrap();
    let user = UserId::new();
    store
        .upsert_unit(
            user, media.id, -1, -1, 1_000, None, None, None, None, None, false, 1,
        )
        .unwrap();

    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();

    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 1);
    assert!(store.get_library(&library.id).unwrap().is_none());
    assert!(store.file_coverage_by_path(&row.path).unwrap().is_none());
    assert!(store.get_media(media.id).unwrap().is_some());
    assert_eq!(
        store
            .unit_state(user, media.id, -1, -1)
            .unwrap()
            .expect("playback facts outlive the Library")
            .position_ms,
        1_000
    );
}

#[test]
fn interruption_between_databases_recovers_original_operation_and_count() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    let (library, row) = fixture(&store, &temp.path().join("tv"));
    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    INTERRUPT_AFTER_LIBRARY_COMMIT.with(|flag| flag.set(true));
    assert!(store.clean_library_removal(&removal.operation_id).is_err());
    assert!(store.file_coverage_by_path(&row.path).unwrap().is_none());
    assert!(store.get_library(&library.id).unwrap().is_some());
    assert!(!store.library_accepts_work(&library.id).unwrap());
    drop(store);
    let store = Store::open(&data).unwrap();
    assert_eq!(
        store
            .begin_library_removal(&library.id)
            .unwrap()
            .operation_id,
        removal.operation_id
    );
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 1);
    assert_eq!(
        store.clean_library_removal(&removal.operation_id).unwrap(),
        done
    );
    assert!(store.get_media(row.media_id).unwrap().is_some());
}

#[test]
fn changed_snapshot_identity_rejects_cleanup_before_any_row_is_removed() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let (library, row) = fixture(&store, &temp.path().join("tv"));
    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    // Simulate a stale writer bypassing the Store freeze barrier. It moves the
    // coverage row and the summary that points at it, so the deferred composite
    // foreign keys stay satisfied and only the snapshot identity goes stale.
    let tx = store.library.unchecked_transaction().unwrap();
    tx.execute(
        "UPDATE library_file_units SET episode=2 WHERE file_id=?1",
        [row.file_id.expect("fixture file id").to_string()],
    )
    .unwrap();
    tx.execute(
        "UPDATE library_units SET episode=2 WHERE library_id=?1 AND media_id=?2",
        params![&library.id, row.media_id.to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.clean_library_removal(&removal.operation_id),
        Err(StoreError::Protected(_))
    ));
    assert!(store.file_coverage_by_path(&row.path).unwrap().is_some());
    assert!(store.get_library(&library.id).unwrap().is_some());
}

#[test]
fn removal_reduces_preferred_file_summaries_and_keeps_media_and_playback() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let (library, row) = fixture(&store, &temp.path().join("tv"));
    let library_id = library.id.parse::<domain::LibraryId>().unwrap();
    let card = store
        .get_library_item(library_id, row.media_id)
        .unwrap()
        .expect("a published file creates a card");
    assert_eq!(
        card.preferred_file_id,
        Some(row.file_id.expect("fixture file id"))
    );
    assert_eq!(summary_rows(&store, &library.id, row.media_id), (1, 1));

    let user = UserId::new();
    store
        .upsert_unit(
            user,
            row.media_id,
            1,
            1,
            120_000,
            Some(false),
            Some(true),
            Some(600_000),
            None,
            None,
            true,
            crate::unix_now(),
        )
        .unwrap();

    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 1);
    assert!(store.get_library(&library.id).unwrap().is_none());
    assert!(
        store
            .get_library_item(library_id, row.media_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(summary_rows(&store, &library.id, row.media_id), (0, 0));

    // Media identity and user playback facts have their own lifetimes.
    assert!(store.get_media(row.media_id).unwrap().is_some());
    let unit = store
        .unit_state(user, row.media_id, 1, 1)
        .unwrap()
        .expect("playback facts outlive the Library");
    assert_eq!(unit.position_ms, 120_000);
    assert_eq!(unit.duration_ms, Some(600_000));
    assert!(unit.favorite);
    assert_eq!(unit.play_count, 1);

    // A retried removal reports the same succeeded operation.
    assert_eq!(
        store.clean_library_removal(&removal.operation_id).unwrap(),
        done
    );
}

/// `(library_items, library_units)` row counts for one media in one library.
fn summary_rows(store: &Store, library_id: &str, media_id: MediaId) -> (i64, i64) {
    let count = |table: &str| {
        store
            .library
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE library_id=?1 AND media_id=?2"),
                params![library_id, media_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    (count("library_items"), count("library_units"))
}

/// 大删除按 ≤100 文件一批各自提交：第一批后崩溃只留下其余文件，重启续跑完成且计数不变。
#[test]
fn large_removal_commits_bounded_batches_and_resumes() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    let (library, first) = fixture(&store, &temp.path().join("tv"));
    let root = temp.path().join("tv");
    for episode in 2..=250 {
        let row = FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: first.media_id,
            path: root
                .join(format!("E{episode:03}.strm"))
                .display()
                .to_string(),
            season: Some(1),
            episode: Some(episode),
            ..FixtureFile::default()
        };
        crate::test_support::publish_fixture_file(&store, &row).unwrap();
    }
    let removal = store.begin_library_removal(&library.id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    INTERRUPT_AFTER_BATCHES.with(|slot| slot.set(Some(1)));
    assert!(store.clean_library_removal(&removal.operation_id).is_err());
    let remaining: i64 = store
        .library
        .query_row(
            "SELECT COUNT(*) FROM library_files WHERE library_id=?1",
            [&library.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 150,
        "first batch of 100 must be durable on its own"
    );
    drop(store);
    let store = Store::open(&data).unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 250);
}
