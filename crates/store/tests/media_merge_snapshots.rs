//! R21: merging two Media must pick the newest whole snapshot of the same provider
//! entity; a tie keeps the survivor's snapshot.
use std::collections::BTreeMap;

use domain::MediaKind;
use rusqlite::{Connection, params};
use store::Store;
use store::playback::MediaMergeRequest;

mod merge_support;
use merge_support::create_media;

const SURVIVOR: &str = "00000000-0000-0000-0000-0000000000a1";
const LOSER: &str = "00000000-0000-0000-0000-0000000000a2";

fn snapshot(db: &Connection, media: &str, entity: &str, json: &str, fetched_at: i64) {
    db.execute(
        "INSERT INTO media_metadata_sources
            (media_id, entity_scope, season, episode, source, locale, provider_entity_id,
             provider_work_id, normalized_json, present_fields_json, preferences_json,
             selection_origins_json, fetched_at, source_revision)
         VALUES (?1, 'work', -1, -1, 'tmdb', 'zh-CN', ?2, ?2, ?3, '[]', '{}', '{}', ?4, 1)",
        params![media, entity, json, fetched_at],
    )
    .unwrap();
}

fn merge_with(survivor: (&str, i64), loser: (&str, i64), loser_entity: &str) -> (String, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    create_media(&store, SURVIVOR, "Survivor", MediaKind::Movie);
    create_media(&store, LOSER, "Loser", MediaKind::Movie);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    snapshot(&db, SURVIVOR, "550", survivor.0, survivor.1);
    snapshot(&db, LOSER, loser_entity, loser.0, loser.1);
    let req = MediaMergeRequest {
        request_id: "00000000-0000-0000-0001-0000000000a1".into(),
        survivor: SURVIVOR.parse().unwrap(),
        loser: LOSER.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), SURVIVOR.to_string())]),
    };
    let op = store.begin_media_merge(&req, 3000).unwrap();
    assert_eq!(store.advance_media_merge(&op, 3000).unwrap().state, "succeeded");
    db.query_row(
        "SELECT normalized_json, fetched_at FROM media_metadata_sources WHERE media_id = ?1",
        params![SURVIVOR],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn newer_same_entity_snapshot_wins() {
    let chosen = merge_with((r#"{"t":"old"}"#, 1200), (r#"{"t":"new"}"#, 2000), "550");
    assert_eq!(chosen, (r#"{"t":"new"}"#.to_string(), 2000));
}

#[test]
fn tied_same_entity_snapshot_keeps_survivor() {
    let chosen = merge_with((r#"{"t":"s"}"#, 1500), (r#"{"t":"l"}"#, 1500), "550");
    assert_eq!(chosen, (r#"{"t":"s"}"#.to_string(), 1500));
}

#[test]
fn different_entity_keeps_refresh_target_snapshot() {
    let chosen = merge_with((r#"{"t":"s"}"#, 1000), (r#"{"t":"l"}"#, 9000), "999");
    assert_eq!(chosen, (r#"{"t":"s"}"#.to_string(), 1000));
}

/// R21: a failed merge keeps its freeze, stays in the recovery list, refuses a newer
/// operation on the same Media, and the same operation completes once retried.
#[test]
fn failed_merge_is_recoverable_and_not_taken_over() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    create_media(&store, SURVIVOR, "Survivor", MediaKind::Movie);
    create_media(&store, LOSER, "Loser", MediaKind::Movie);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    snapshot(&db, LOSER, "550", "{}", 1);
    db.execute_batch(
        "CREATE TRIGGER fail_merge BEFORE UPDATE OF media_id ON media_metadata_sources
         BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let req = |id: &str| MediaMergeRequest {
        request_id: id.into(),
        survivor: SURVIVOR.parse().unwrap(),
        loser: LOSER.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), SURVIVOR.to_string())]),
    };
    let op = store.begin_media_merge(&req("00000000-0000-0000-0001-0000000000b1"), 1000).unwrap();
    assert!(store.advance_media_merge(&op, 1100).is_err());
    let failed_operation_is_recoverable = store.unfinished_media_merges().unwrap() == vec![op.clone()];
    assert!(failed_operation_is_recoverable);
    assert!(
        store.begin_media_merge(&req("00000000-0000-0000-0001-0000000000b2"), 1200).is_err(),
        "a newer operation must not take over the failed one's freeze"
    );
    db.execute_batch("DROP TRIGGER fail_merge;").unwrap();
    assert_eq!(store.advance_media_merge(&op, 1300).unwrap().state, "succeeded");
    assert!(store.unfinished_media_merges().unwrap().is_empty());
}

fn override_row(db: &Connection, media: &str, fields: &str) {
    db.execute(
        "INSERT INTO media_metadata_overrides
            (media_id, entity_scope, season, episode, locale, fields_json, revision)
         VALUES (?1, 'work', -1, -1, 'zh-CN', ?2, 1)",
        params![media, fields],
    )
    .unwrap();
}

/// R21: conflicting user locks refuse the merge with zero writes; disjoint locks merge.
#[test]
fn conflicting_override_begin_is_refused_and_disjoint_overrides_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    create_media(&store, SURVIVOR, "Survivor", MediaKind::Movie);
    create_media(&store, LOSER, "Loser", MediaKind::Movie);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    override_row(&db, SURVIVOR, r#"{"title":"S"}"#);
    override_row(&db, LOSER, r#"{"title":"L","overview":"LO"}"#);
    let req = MediaMergeRequest {
        request_id: "00000000-0000-0000-0001-0000000000c1".into(),
        survivor: SURVIVOR.parse().unwrap(),
        loser: LOSER.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), SURVIVOR.to_string())]),
    };
    let conflicting_override_begin = store.begin_media_merge(&req, 1000);
    assert!(
        matches!(&conflicting_override_begin, Err(store::StoreError::Protected(m)) if m.contains("lock field title")),
        "{conflicting_override_begin:?}"
    );
    assert!(store.unfinished_media_merges().unwrap().is_empty(), "zero writes");

    db.execute(
        "UPDATE media_metadata_overrides SET fields_json = '{\"overview\":\"LO\"}' WHERE media_id = ?1",
        params![LOSER],
    )
    .unwrap();
    let op = store.begin_media_merge(&req, 1100).unwrap();
    assert_eq!(store.advance_media_merge(&op, 1200).unwrap().state, "succeeded");
    let merged: String = db
        .query_row(
            "SELECT fields_json FROM media_metadata_overrides WHERE media_id = ?1",
            params![SURVIVOR],
            |r| r.get(0),
        )
        .unwrap();
    let merged: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(merged, serde_json::json!({"title": "S", "overview": "LO"}));
}
