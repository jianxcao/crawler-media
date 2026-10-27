//! The identity write must skip exactly the writes that change nothing.
//!
//! `Store::update_media` adopts a Media once per scanned file, and each adoption used to
//! rewrite the row and rebuild every summary derived from it (`reduce_media_everywhere`
//! re-reads every published file of that Media). The no-op guard is what makes a rescan cheap;
//! these cases fix its boundaries: identical input writes nothing, and every adjacent case
//! (`changed column`, `missing alias row`, `stale card identity_state`) still writes.
//!
//! Deterministic currency: rows SQLite actually wrote (`total_changes`) and statements the
//! closure executed (`Store::with_stats`). Measured: 7 statements for the skip, 43 for a
//! rebuild. No wall clock.

use super::*;
use crate::stats::StatsScope;
use domain::{LibraryId, MediaKind};

#[cfg(test)]
impl Store {
    /// Rows this Store's `library.db` connection has written (SQLite's own counter).
    fn test_total_changes(&self) -> u64 {
        self.library.total_changes()
    }

    fn test_execute(&self, sql: &str, id: &str) -> Result<(), StoreError> {
        self.library.execute(sql, [id])?;
        Ok(())
    }

    fn test_scalar<T: rusqlite::types::FromSql>(&self, sql: &str, id: &str) -> T {
        self.library.query_row(sql, [id], |row| row.get(0)).unwrap()
    }
}



fn temp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    (dir, store)
}

fn media(title: &str, tmdb: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.to_string(),
        year: Some(2020),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: tmdb.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

/// One published file of `media_id` in a fresh Library, so a summary rebuild would have work.
fn publish_one_file(store: &Store, media_id: MediaId) -> (tempfile::TempDir, LibraryId) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
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
    let library_id: LibraryId = library.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|row| row.library_id == library.id)
        .unwrap()
        .id;
    let file = domain::LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id,
        root_id,
        media_id,
        kind: MediaKind::Tv,
        path: root.join("Series S01E01.mkv"),
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(1),
        modified_ns: Some(2),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
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
            &crate::library_write::WriteScope {
                library_id,
                config_revision: store.library_config_revision(&library.id).unwrap(),
            },
            &[crate::library_write::FilePublication {
                file: &file,
                units: &[domain::UnitKey::new(1, 1)],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    (dir, library_id)
}

fn assert_no_op(store: &Store, media: &Media) {
    let before = store.test_total_changes();
    let (result, stats) = store.with_stats(StatsScope::Library, || store.update_media(media));
    result.unwrap();
    assert_eq!(
        store.test_total_changes(),
        before,
        "an identical identity write must not write a row"
    );
    // Measured: 7 statements for the skip, 43 for a rebuild (which also touches 31 rows
    // across cards, terms and user summaries). The bound is what "no rebuild" means.
    assert!(
        stats.statements <= 8,
        "an identical identity write must not rebuild summaries: {} statements",
        stats.statements
    );
}

#[test]
fn identical_identity_is_a_no_op() {
    let (_dir, store) = temp_store();
    let media = media("Series", Some("603"));
    store.insert_media(&media).unwrap();
    let (_files, _library) = publish_one_file(&store, media.id);
    let stored = store.get_media(media.id).unwrap().expect("media");

    assert_no_op(&store, &stored);
    // Twice: the guard cannot be defeated by its own first call.
    assert_no_op(&store, &stored);
}

#[test]
fn changed_identity_still_writes_and_republishes() {
    let (_dir, store) = temp_store();
    let media = media("Series", Some("603"));
    store.insert_media(&media).unwrap();
    let (_files, library_id) = publish_one_file(&store, media.id);

    let mut renamed = store.get_media(media.id).unwrap().expect("media");
    renamed.title = "Renamed".into();
    renamed.year = Some(2021);
    let (_, stats) = store.with_stats(StatsScope::Library, || store.update_media(&renamed));
    assert!(
        stats.statements > 20,
        "a real identity change must still rewrite the row and republish: {} statements",
        stats.statements
    );
    assert_eq!(
        store.get_media(media.id).unwrap().unwrap().title,
        "Renamed"
    );
    // The summaries follow the identity in the same commit, and they are still reachable.
    let card = store
        .get_library_item(library_id, media.id)
        .unwrap()
        .expect("card");
    assert_eq!(card.title, "Renamed");
    assert_eq!(card.file_count, 1);
}

#[test]
fn a_missing_alias_row_is_not_a_no_op() {
    let (_dir, store) = temp_store();
    let media = media("Series", Some("603"));
    store.insert_media(&media).unwrap();
    let stored = store.get_media(media.id).unwrap().expect("media");

    // The row still carries the id, but the alias row is gone: the writer would restore it,
    // so this must not be treated as identical.
    store
        .test_execute("DELETE FROM media_aliases WHERE media_id = ?1", &media.id.to_string())
        .unwrap();
    store.update_media(&stored).unwrap();
    let aliases: i64 = store.test_scalar(
        "SELECT COUNT(*) FROM media_aliases WHERE media_id = ?1",
        &media.id.to_string(),
    );
    assert_eq!(aliases, 1, "a missing alias row must be restored");
}

#[test]
fn a_stale_card_identity_state_is_not_a_no_op() {
    let (_dir, store) = temp_store();
    let media = media("Series", Some("603"));
    store.insert_media(&media).unwrap();
    let (_files, _library) = publish_one_file(&store, media.id);
    let stored = store.get_media(media.id).unwrap().expect("media");

    // The writer copies `identity_state` onto every card; a card that disagrees with the
    // row is exactly the case a naive "row matches" guard would leave behind.
    store
        .test_execute(
            "UPDATE library_items SET identity_state = 'provisional' WHERE media_id = ?1",
            &media.id.to_string(),
        )
        .unwrap();
    let before = store.test_total_changes();
    store.update_media(&stored).unwrap();
    assert!(
        store.test_total_changes() > before,
        "a card whose identity_state disagrees must be rewritten"
    );
    let state: String = store.test_scalar(
        "SELECT identity_state FROM library_items WHERE media_id = ?1",
        &media.id.to_string(),
    );
    assert_eq!(state, "confirmed");
}
