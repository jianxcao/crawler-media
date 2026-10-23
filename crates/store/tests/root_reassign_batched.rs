//! A6: nested-root reassignment moves files in batches of ≤100, one transaction each,
//! and resumes from a durable cursor after a crash.
use domain::{Media, MediaId, MediaKind};
use store::Store;
use store::test_support::{
    FixtureFile, pending_root_reassignments, publish_fixture_file, set_root_reassign_batch_budget,
};

fn media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "M".into(),
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
    id
}

fn owned_by(store: &Store, library: &str) -> usize {
    store::test_support::library_file_count(store, library)
}

#[test]
fn nested_root_reassigns_in_bounded_batches_and_resumes_after_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let parent = tmp.path().join("p");
    let nested = parent.join("n");
    std::fs::create_dir_all(&nested).unwrap();
    let (parent_id, nested_id);
    {
        let store = Store::open(&data).unwrap();
        let lib = store
            .create_library(
                MediaKind::Movie,
                "P",
                &[parent.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        parent_id = lib.id.clone();
        for i in 0..250 {
            let path = nested.join(format!("m{i:03}.mkv"));
            std::fs::write(&path, b"x").unwrap();
            let m = media(&store);
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id: m,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(owned_by(&store, &parent_id), 250);
        // Crash after the first batch commits.
        set_root_reassign_batch_budget(Some(1));
        let lib = store
            .create_library(
                MediaKind::Movie,
                "N",
                &[nested.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        set_root_reassign_batch_budget(None);
        nested_id = lib.id.clone();
        assert_eq!(
            owned_by(&store, &nested_id),
            100,
            "one batch of 100 committed"
        );
        assert_eq!(owned_by(&store, &parent_id), 150);
        assert_eq!(
            pending_root_reassignments(&store).len(),
            1,
            "durable cursor kept"
        );
    }
    let store = Store::open(&data).unwrap();
    let batches = store.drain_root_reassignments().unwrap();
    assert_eq!(batches, 2, "resumes from the cursor: 100 + 50");
    assert_eq!(owned_by(&store, &nested_id), 250);
    assert_eq!(owned_by(&store, &parent_id), 0);
    assert!(pending_root_reassignments(&store).is_empty());
}
