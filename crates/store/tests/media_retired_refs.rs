use std::collections::BTreeMap;

use domain::MediaKind;
use store::Store;
use store::playback::MediaMergeRequest;

#[path = "merge_support/mod.rs"]
mod merge_support;
use merge_support::create_media;

#[test]
fn retired_media_survivors_returns_loser_and_survivor_pairs_after_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";
    let m3_id = "00000000-0000-0000-0000-000000000003";

    let survivor = create_media(&store, m1_id, "Survivor Show", MediaKind::Tv);
    let loser = create_media(&store, m2_id, "Loser Show Duplicate", MediaKind::Tv);
    let _unmerged = create_media(&store, m3_id, "Unmerged Show", MediaKind::Tv);

    // Before merge, no retired media
    let before = store.retired_media_survivors().unwrap();
    assert!(before.is_empty());

    let canonical_req_id = "00000000-0000-0000-0001-000000000001".to_string();
    let req = MediaMergeRequest {
        request_id: canonical_req_id,
        survivor: survivor.id,
        loser: loser.id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };

    let op_id = store
        .begin_media_merge(&req, 1000)
        .expect("begin_media_merge");
    let status = store
        .advance_media_merge(&op_id, 1000)
        .expect("advance_media_merge");
    assert_eq!(status.state, "succeeded");

    let after = store.retired_media_survivors().unwrap();
    assert_eq!(after, vec![(loser.id, survivor.id)]);
}
