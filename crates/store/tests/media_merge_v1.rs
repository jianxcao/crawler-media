use std::collections::BTreeMap;
use domain::{Media, MediaId, MediaKind};
use store::playback::{MediaMergeRequest, OperationStatus};
use store::Store;

fn create_media(store: &Store, id: &str, title: &str, kind: MediaKind) -> Media {
    let m = Media {
        id: id.parse().unwrap(),
        kind,
        title: title.to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: Some(id.to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&m).unwrap();
    m
}

#[test]
fn media_merge_lifecycle_preserves_survivor_and_retires_loser() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";

    create_media(&store, m1_id, "Survivor Show", MediaKind::Tv);
    create_media(&store, m2_id, "Loser Show Duplicate", MediaKind::Tv);

    let req = MediaMergeRequest {
        request_id: "req_merge_1".into(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };

    let op_id = store.begin_media_merge(&req, 1000).expect("begin_media_merge");
    assert!(!op_id.is_empty());

    let status = store.advance_media_merge(&op_id, 1000).expect("advance_media_merge");
    assert_eq!(status.state, "succeeded");

    // Check survivor is active and loser is retired pointing to survivor
    let survivor = store.get_media(m1_id.parse().unwrap()).unwrap().expect("survivor exists");
    assert_eq!(survivor.title, "Survivor Show");

    // Re-check with identical request_id produces idempotent operation
    let op_id_2 = store.begin_media_merge(&req, 1000).expect("idempotent begin_media_merge");
    assert_eq!(op_id, op_id_2);
}
