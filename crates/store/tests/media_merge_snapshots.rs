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
