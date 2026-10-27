//! N05: every report sample list is capped at REPORT_CAP while exact totals
//! are kept separately, for any batch size and across resumed runs.
use domain::{LibraryId, Media, MediaId, MediaKind};
use rusqlite::Connection;
use store::Store;
use store::library_operations::integrity::REPORT_CAP;
use store::test_support::{FixtureFile, publish_fixture_files};

const MEDIA: usize = 150;

fn fixture() -> (tempfile::TempDir, Store, LibraryId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "m",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let mut specs = Vec::new();
    for n in 0..MEDIA {
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Movie,
                title: format!("M{n}"),
                year: Some(2000),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = root.join(format!("M{n}.mkv"));
        std::fs::write(&path, b"x").unwrap();
        specs.push(FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            ..Default::default()
        });
    }
    publish_fixture_files(&store, &specs).unwrap();
    let db = Connection::open(tmp.path().join("data/library.db")).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys = OFF;
         DELETE FROM library_item_terms; DELETE FROM library_units; DELETE FROM library_items;",
    )
    .unwrap();
    (tmp, store, lib.id.parse().unwrap())
}

#[test]
fn report_samples_are_capped_and_totals_exact_for_any_batch() {
    let (tmp, store, library) = fixture();
    for (batch, op) in [(1usize, "b1"), (100, "b100")] {
        store
            .begin_library_integrity(library, false, op, 1)
            .unwrap();
        for step in 0.. {
            let status = store
                .advance_library_operation(op, 2 + step, batch)
                .unwrap();
            if status.state != "applying" {
                break;
            }
        }
        let report = store.library_integrity_report(op).unwrap();
        assert!(report.missing_items.len() <= REPORT_CAP);
        assert!(report.mismatched_media.len() <= REPORT_CAP);
        assert_eq!(report.totals.missing_items, MEDIA, "{batch}");
        assert_eq!(report.totals.mismatched_media, MEDIA, "{batch}");
        assert!(report.truncated);
        let db = Connection::open(tmp.path().join("data/library.db")).unwrap();
        let size: i64 = db
            .query_row(
                "SELECT length(cursor_json) FROM library_operations WHERE operation_id = ?1",
                [op],
                |r| r.get(0),
            )
            .unwrap();
        assert!(size < 32 * 1024, "cursor {size} bytes must stay bounded");
    }
}
