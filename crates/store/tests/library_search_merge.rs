//! Merge removes the retired card's search postings in the same transaction.
use domain::{Media, MediaId, MediaKind};
use std::collections::BTreeMap;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Store, playback::MediaMergeRequest};

#[test]
fn search_merge_keeps_survivor_and_removes_retired_card_postings() {
    let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
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
    let library = library.id.parse().unwrap();
    let survivor = MediaId::new();
    let loser = MediaId::new();
    for (id, title) in [(survivor, "Survivor Beacon"), (loser, "Retired Lantern")] {
        store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: title.into(),
                year: None,
                original_title: None,
                tmdb_id: Some(id.to_string()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        let file = root.join(format!("{id}.mkv"));
        std::fs::write(&file, b"movie").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: id,
                path: file.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(
        store
            .search_library_items(&[library], "lantern", None, None)
            .unwrap()
            .items[0]
            .media_id,
        loser
    );
    let operation = store
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: BTreeMap::from([("tmdb".into(), survivor.to_string())]),
            },
            1000,
        )
        .unwrap();
    assert_eq!(
        store.advance_media_merge(&operation, 2000).unwrap().state,
        "succeeded"
    );
    let hits = store
        .search_library_items(&[library], "beacon", None, None)
        .unwrap();
    assert_eq!(hits.items.len(), 1);
    assert_eq!(hits.items[0].media_id, survivor);
    assert!(
        store
            .search_library_items(&[library], "lantern", None, None)
            .unwrap()
            .items
            .is_empty()
    );
    store
        .begin_library_integrity(library, false, "merge-search-verify", 3000)
        .unwrap();
    for step in 1..100 {
        let status = store
            .advance_library_operation("merge-search-verify", 3000 + step, 1)
            .unwrap();
        assert_ne!(status.state, "failed", "{status:?}");
        if status.state == "succeeded" {
            let report = store
                .library_integrity_report("merge-search-verify")
                .unwrap();
            assert!(!report.has_issues(), "{report:#?}");
            return;
        }
    }
    panic!("finite merge integrity check did not terminate");
}
