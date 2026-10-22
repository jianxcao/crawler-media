//! R20/S02: the checker compares every derived fact (units, terms, counts, user rows)
//! with what the raw facts reduce to, page by page, and repair re-verifies each Media.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaKind, QualitySource, UnitKey,
};
use rusqlite::{Connection, params};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn setup() -> (tempfile::TempDir, Store, LibraryId, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let lib = store
        .create_library(MediaKind::Movie, "Movies", &["/movies"], "everyone", true, &[])
        .unwrap();
    let library_id: LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_deref() == Some(lib.id.as_str()))
        .unwrap()
        .id;
    let mut ids = Vec::new();
    for n in 1..=3 {
        let id = format!("00000000-0000-0000-0000-00000000000{n}");
        let media = Media {
            id: id.parse().unwrap(),
            kind: MediaKind::Movie,
            title: format!("Movie {n}"),
            year: Some(2024),
            original_title: None,
            tmdb_id: Some(id.clone()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: vec![],
        };
        store.insert_media(&media).unwrap();
        let file = LibraryFile {
            file_id: LedgerId::new(),
            library_id,
            root_id: root_id.clone(),
            media_id: media.id,
            kind: MediaKind::Movie,
            path: format!("/movies/m{n}.mkv").into(),
            version: FileVersion { revision: 1, source_version: "v1".into() },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1024),
            stat_known: true,
            created_ns: Some(0),
            modified_ns: Some(0),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        };
        store
            .publish_files(
                &WriteScope { library_id, config_revision: 0 },
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
        ids.push(id);
    }
    (tmp, store, library_id, ids)
}

fn run(store: &Store, library: LibraryId, repair: bool, op: &str) -> (String, usize) {
    store.begin_library_integrity(library, repair, op, 2000).unwrap();
    let mut calls = 0;
    loop {
        calls += 1;
        let status = store.advance_library_operation(op, 2000 + calls as i64, 1).unwrap();
        if status.state != "applying" {
            return (status.state, calls);
        }
        assert!(calls < 50);
    }
}

fn integrity_state(db: &Connection, library: LibraryId) -> String {
    db.query_row(
        "SELECT integrity_state FROM libraries WHERE library_id = ?1",
        params![library.to_string()],
        |r| r.get(0),
    )
    .unwrap()
}

fn units_and_terms(db: &Connection, media: &str) -> (i64, i64, i64) {
    db.query_row(
        "SELECT (SELECT COUNT(*) FROM library_units WHERE media_id = ?1),
                (SELECT COUNT(*) FROM library_item_terms WHERE media_id = ?1),
                (SELECT file_count FROM library_items WHERE media_id = ?1)",
        params![media],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

#[test]
fn missing_units_and_terms_are_detected() {
    let (tmp, store, library, ids) = setup();
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let expected_from_raw = units_and_terms(&db, &ids[1]);
    let (state, _) = run(&store, library, false, "op-clean");
    assert_eq!(state, "succeeded");
    assert_eq!(integrity_state(&db, library), "clean");
    assert!(!store.library_integrity_report("op-clean").unwrap().has_issues());

    db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    db.execute("DELETE FROM library_units WHERE media_id = ?1", params![ids[0]]).unwrap();
    db.execute("DELETE FROM library_item_terms WHERE media_id = ?1", params![ids[1]]).unwrap();
    db.execute("UPDATE library_items SET size_bytes = 1, size_sort = 1 WHERE media_id = ?1", params![ids[2]])
        .unwrap();

    let (state, calls) = run(&store, library, false, "op-check");
    assert_eq!(state, "succeeded");
    assert!(calls >= 3, "one Media per batch, never the whole Library at once");
    assert_eq!(integrity_state(&db, library), "failed");
    let damaged_report = store.library_integrity_report("op-check").unwrap();
    assert!(damaged_report.has_issues());
    assert_eq!(damaged_report.mismatched_media.len(), 3, "{damaged_report:?}");

    let (state, _) = run(&store, library, true, "op-repair");
    assert_eq!(state, "succeeded");
    assert_eq!(integrity_state(&db, library), "clean");
    let repaired_facts = units_and_terms(&db, &ids[1]);
    assert_eq!(repaired_facts, expected_from_raw);
    assert_eq!(units_and_terms(&db, &ids[0]).0, 1);
}
