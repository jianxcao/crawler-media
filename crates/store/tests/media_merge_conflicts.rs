use std::collections::BTreeMap;

use domain::{MediaId, MediaKind};
use rusqlite::{Connection, params};
use store::playback::MediaMergeRequest;
use store::{Store, StoreError};

mod merge_support;
use merge_support::{create_media, insert_alias};

#[test]
fn self_merge_is_rejected_before_operation_or_fact_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let id = "00000000-0000-0000-0000-000000000001";
    let media = create_media(&store, id, "Keep identity", MediaKind::Movie);
    let request_id = "00000000-0000-0000-0001-000000000001".to_string();
    let request = MediaMergeRequest {
        request_id: request_id.clone(),
        survivor: media.id,
        loser: media.id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    assert!(matches!(
        store.begin_media_merge(&request, 1),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store.get_media(media.id).unwrap().unwrap().title,
        "Keep identity"
    );

    // Zero writes: media_operations has no rows, media identity_state and metadata_revision unchanged
    let db = Connection::open(temp.path().join("library.db")).unwrap();
    let op_count: i64 = db
        .query_row("SELECT COUNT(*) FROM media_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(op_count, 0);

    let (state, rev): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![media.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "confirmed");
    assert_eq!(rev, 0);
}

#[test]
fn different_kind_merge_is_rejected_with_zero_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, m1_id, "Movie Survivor", MediaKind::Movie);
    create_media(&store, m2_id, "TV Loser", MediaKind::Tv);

    let request_id = "00000000-0000-0000-0001-000000000002".to_string();
    let request = MediaMergeRequest {
        request_id: request_id.clone(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    let err = store.begin_media_merge(&request, 1).unwrap_err();
    assert!(
        matches!(&err, StoreError::Protected(msg) if msg.contains("different kinds")),
        "expected Protected error for different kinds, got {err:?}"
    );

    let db = Connection::open(temp.path().join("library.db")).unwrap();
    let op_count: i64 = db
        .query_row("SELECT COUNT(*) FROM media_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(op_count, 0);

    let (s1, r1): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![m1_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s1, "confirmed");
    assert_eq!(r1, 0);
    let (s2, r2): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![m2_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s2, "confirmed");
    assert_eq!(r2, 0);
}

#[test]
fn revision_mismatch_is_rejected_with_zero_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, m1_id, "Survivor", MediaKind::Tv);
    create_media(&store, m2_id, "Loser", MediaKind::Tv);

    // Mismatched survivor_revision (expected 0, pass 99)
    let request_id = "00000000-0000-0000-0001-000000000003".to_string();
    let request = MediaMergeRequest {
        request_id: request_id.clone(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 99,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };
    let err = store.begin_media_merge(&request, 1).unwrap_err();
    assert!(
        matches!(&err, StoreError::Conflict(message) if message.contains("revision mismatch")),
        "expected Conflict with revision mismatch, got {err:?}"
    );

    let db = Connection::open(temp.path().join("library.db")).unwrap();
    let op_count: i64 = db
        .query_row("SELECT COUNT(*) FROM media_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(op_count, 0);

    let (s1, r1): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![m1_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s1, "confirmed");
    assert_eq!(r1, 0);
    let (s2, r2): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![m2_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s2, "confirmed");
    assert_eq!(r2, 0);
}

#[test]
fn media_merge_requires_a_refresh_target_decision_and_refuses_frozen_survivors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let survivor_id = "00000000-0000-0000-0000-0000000000b1";
    let loser_id = "00000000-0000-0000-0000-0000000000b2";
    create_media(&store, survivor_id, "Survivor", MediaKind::Tv);
    create_media(&store, loser_id, "Loser", MediaKind::Tv);
    let survivor: MediaId = survivor_id.parse().unwrap();
    let loser: MediaId = loser_id.parse().unwrap();

    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "DELETE FROM media_aliases WHERE media_id IN (?1, ?2)",
        params![survivor_id, loser_id],
    )
    .unwrap();
    insert_alias(&db, survivor_id, "tmdb", "external-a", 1);
    insert_alias(&db, loser_id, "tmdb", "external-b", 1);

    let undecided_id = "00000000-0000-0000-0002-000000000001".to_string();
    let undecided = MediaMergeRequest {
        request_id: undecided_id,
        survivor,
        loser,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    // Undecided refresh target must fail at begin_media_merge
    let err = store.begin_media_merge(&undecided, 1000).unwrap_err();
    assert!(
        matches!(&err, StoreError::Protected(message) if message.contains("refresh_targets[tmdb]")),
        "unexpected error: {err:?}"
    );

    // Zero writes on refusal
    let op_count: i64 = db
        .query_row("SELECT COUNT(*) FROM media_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(op_count, 0);
    let (s1, _): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![survivor_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s1, "confirmed");
    let (s2, _): (String, i64) = db
        .query_row(
            "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
            params![loser_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s2, "confirmed");

    // Decided version succeeds
    let mut refresh_targets = BTreeMap::new();
    refresh_targets.insert("tmdb".to_string(), "external-b".to_string());
    let decided_id = "00000000-0000-0000-0002-000000000002".to_string();
    let decided = MediaMergeRequest {
        request_id: decided_id,
        survivor,
        loser,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets,
    };
    let op = store.begin_media_merge(&decided, 1000).unwrap();
    let status = store.advance_media_merge(&op, 1000).unwrap();
    assert_eq!(status.state, "succeeded");

    let (targets, target_external): (i64, Option<String>) = db
        .query_row(
            "SELECT COUNT(*), MAX(external_id) FROM media_aliases
             WHERE media_id = ?1 AND source = 'tmdb' AND kind = 'tv' AND is_refresh_target = 1",
            params![survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (targets, target_external.as_deref()),
        (1, Some("external-b"))
    );
}

#[test]
fn same_request_id_with_different_payload_returns_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";
    let m3_id = "00000000-0000-0000-0000-000000000003";
    create_media(&store, m1_id, "Survivor", MediaKind::Tv);
    create_media(&store, m2_id, "Loser 1", MediaKind::Tv);
    create_media(&store, m3_id, "Loser 2", MediaKind::Tv);

    let shared_request_id = "00000000-0000-0000-0003-000000000001".to_string();
    let req1 = MediaMergeRequest {
        request_id: shared_request_id.clone(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };
    let op1 = store.begin_media_merge(&req1, 1000).unwrap();

    // New behavior: operation_id == request_id
    assert_eq!(op1, shared_request_id);

    // Same request_id, identical payload -> idempotent success returning same operation_id
    let op1_repeat = store.begin_media_merge(&req1, 1000).unwrap();
    assert_eq!(op1_repeat, shared_request_id);

    // Same request_id, different payload (different loser) -> Conflict
    let req2 = MediaMergeRequest {
        request_id: shared_request_id.clone(),
        survivor: m1_id.parse().unwrap(),
        loser: m3_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };
    let err = store.begin_media_merge(&req2, 1000).unwrap_err();
    assert!(
        matches!(&err, StoreError::Conflict(msg) if msg.contains("different plan")),
        "expected Conflict error on different payload, got {err:?}"
    );

    let db = Connection::open(temp.path().join("library.db")).unwrap();
    let (op_count, op_state): (i64, String) = db
        .query_row(
            "SELECT COUNT(*), MAX(state) FROM media_operations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(op_count, 1);
    assert_eq!(op_state, "frozen");

    // m3 was not modified by the rejected second request
    let m3_state: String = db
        .query_row(
            "SELECT identity_state FROM media WHERE media_id = ?1",
            params![m3_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(m3_state, "confirmed");
}

#[test]
fn frozen_survivor_held_by_another_incomplete_operation_returns_protected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, m1_id, "Survivor", MediaKind::Tv);
    create_media(&store, m2_id, "Loser", MediaKind::Tv);

    let db = Connection::open(temp.path().join("library.db")).unwrap();
    let foreign_op_id = "00000000-0000-0000-0004-000000000099".to_string();
    db.execute(
        "INSERT INTO media_operations
            (operation_id, kind, state, survivor_id, loser_id, plan_json, phase_cursor, error, created_at_ms, updated_at_ms)
         VALUES (?1, 'merge', 'frozen', ?2, NULL, '{}', 'init', NULL, 1000, 1000)",
        params![foreign_op_id, m1_id],
    )
    .unwrap();
    db.execute(
        "UPDATE media SET identity_state = 'merging' WHERE media_id = ?1",
        params![m1_id],
    )
    .unwrap();

    let req_id = "00000000-0000-0000-0004-000000000001".to_string();
    let req = MediaMergeRequest {
        request_id: req_id,
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };
    let err = store.begin_media_merge(&req, 1000).unwrap_err();
    assert!(
        matches!(&err, StoreError::Protected(msg) if msg.contains("frozen by merge operation")),
        "expected Protected error on merging media held by foreign op, got {err:?}"
    );

    // Only 1 operation row (the foreign one), no new operation inserted
    let op_count: i64 = db
        .query_row("SELECT COUNT(*) FROM media_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(op_count, 1);

    // Reverse test: owner operation advance can resume without being blocked by its own freeze
    let owner_req_id = "00000000-0000-0000-0004-000000000002".to_string();
    let m3_id = "00000000-0000-0000-0000-000000000003";
    let m4_id = "00000000-0000-0000-0000-000000000004";
    create_media(&store, m3_id, "Survivor 2", MediaKind::Tv);
    create_media(&store, m4_id, "Loser 2", MediaKind::Tv);
    let owner_req = MediaMergeRequest {
        request_id: owner_req_id.clone(),
        survivor: m3_id.parse().unwrap(),
        loser: m4_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m3_id.to_string())]),
    };
    let op = store.begin_media_merge(&owner_req, 1000).unwrap();
    assert_eq!(op, owner_req_id);
    let status = store.advance_media_merge(&op, 1000).unwrap();
    assert_eq!(status.state, "succeeded");
}
