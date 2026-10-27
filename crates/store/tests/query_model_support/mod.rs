#![allow(dead_code)]
//! R20/S02: the checker compares every derived fact (units, terms, counts, user rows)
//! with what the raw facts reduce to, page by page, and repair re-verifies each Media.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaKind, QualitySource, UnitKey,
};
use rusqlite::{Connection, params};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

pub fn setup() -> (tempfile::TempDir, Store, LibraryId, Vec<String>) {
    setup_count(4)
}

pub fn setup_count(count: usize) -> (tempfile::TempDir, Store, LibraryId, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let lib = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let library_id: LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap()
        .id;
    let mut ids = Vec::new();
    for n in 1..=count {
        let id = format!("00000000-0000-0000-0000-{n:012}");
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
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
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
                &WriteScope {
                    library_id,
                    config_revision: 0,
                },
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
        if n <= 2 {
            let fields = serde_json::from_value(serde_json::json!({"genres":{"state":"value","value":[{"source":"fixture","name":"Drama","filter_key":"Drama"}]}})).unwrap();
            store
                .set_metadata_overrides(media.id, domain::EntityScope::Work, "en-US", &fields, 0)
                .unwrap();
        }
        ids.push(id);
    }
    (tmp, store, library_id, ids)
}

pub fn run(store: &Store, library: LibraryId, repair: bool, op: &str) -> (String, usize) {
    store
        .begin_library_integrity(library, repair, op, 2000)
        .unwrap();
    let mut calls = 0;
    loop {
        calls += 1;
        let status = store
            .advance_library_operation(op, 2000 + calls as i64, 1)
            .unwrap();
        if status.state != "applying" {
            return (status.state, calls);
        }
        assert!(calls < 1000);
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
