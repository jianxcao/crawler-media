use std::collections::BTreeMap;

use domain::MediaKind;
use rusqlite::{Connection, params};
use store::Store;
use store::playback::MediaMergeRequest;

mod merge_support;
use merge_support::{
    MergeFixture, adopted_episodes, create_media, insert_nfo_input, read_summary, setup_merge,
    setup_pending_merge,
};

#[test]
fn media_merge_lifecycle_preserves_survivor_and_retires_loser() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";

    create_media(&store, m1_id, "Survivor Show", MediaKind::Tv);
    create_media(&store, m2_id, "Loser Show Duplicate", MediaKind::Tv);

    let canonical_req_id = "00000000-0000-0000-0001-000000000001".to_string();
    let req = MediaMergeRequest {
        request_id: canonical_req_id.clone(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
    };

    let op_id = store
        .begin_media_merge(&req, 1000)
        .expect("begin_media_merge");
    assert!(!op_id.is_empty());

    let status = store
        .advance_media_merge(&op_id, 1000)
        .expect("advance_media_merge");
    assert_eq!(status.state, "succeeded");

    // Check survivor is active and loser is retired pointing to survivor
    let survivor = store
        .get_media(m1_id.parse().unwrap())
        .unwrap()
        .expect("survivor exists");
    assert_eq!(survivor.title, "Survivor Show");

    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let (s_state, s_survivor): (String, Option<String>) = db
        .query_row(
            "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
            params![m1_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(s_state, "confirmed");
    assert_eq!(s_survivor, None);

    let (l_state, l_survivor): (String, Option<String>) = db
        .query_row(
            "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
            params![m2_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(l_state, "retired");
    assert_eq!(l_survivor, Some(m1_id.to_string()));

    let op_id_2 = store
        .begin_media_merge(&req, 1000)
        .expect("idempotent begin_media_merge");
    assert_eq!(op_id, op_id_2);
}

#[test]
fn media_merge_moves_files_facts_collections_and_subscriptions() {
    let f = setup_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    assert_files_and_tombstone(&f, &db);
    assert_playback_facts(&f, &db);
    assert_collections_and_aliases(&f, &db);
    assert_metadata_merge(&f, &db);
    assert_subscriptions_and_idempotent_rerun(&f, &db);
}

fn assert_files_and_tombstone(f: &MergeFixture, db: &Connection) {
    let detail = f
        .store
        .library_item_detail(f.library, f.survivor, "zh-CN")
        .unwrap()
        .expect("survivor card");
    assert_eq!(detail.item.file_count, 2);
    assert!(
        detail
            .files
            .iter()
            .any(|file| file.file_id.to_string() == f.loser_file),
        "loser file moved onto the survivor card"
    );

    let (identity, survivor_ref): (String, Option<String>) = db
        .query_row(
            "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
            params![f.loser.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(identity, "retired");
    assert_eq!(survivor_ref, Some(f.survivor.to_string()));
}

fn assert_playback_facts(f: &MergeFixture, db: &Connection) {
    let merged: (
        i64,
        i64,
        Option<i64>,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
    ) = db
        .query_row(
            "SELECT played, has_played_assertion, played_changed_at_ms, favorite, favorite_at,
                    play_count, watch_activity_at, position_ms, resume_file_id
             FROM playback_units
             WHERE user_id = '00000000-0000-0000-0002-000000000001'
               AND media_id = ?1 AND season = -1 AND episode = -1",
            params![f.survivor.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(merged.0, 1, "played assertion OR");
    assert_eq!(merged.1, 1);
    assert_eq!(
        merged.2,
        Some(30),
        "played_changed_at_ms keeps the max assertion clock"
    );
    assert_eq!(merged.3, 1, "favourite OR");
    assert_eq!(merged.4, 10, "favorite_at keeps the later timestamp");
    assert_eq!(merged.5, 2, "play_count is max, never summed");
    assert_eq!(merged.6, 30);
    assert_eq!(
        merged.7, 7_000,
        "resume package comes whole from the later row"
    );
    assert_eq!(merged.8.as_deref(), Some(f.loser_file.as_str()));

    let (played_b, has_b, changed_b, favorite_b): (i64, i64, Option<i64>, i64) = db
        .query_row(
            "SELECT played, has_played_assertion, played_changed_at_ms, favorite
             FROM playback_units
             WHERE user_id = '00000000-0000-0000-0002-000000000002'
               AND media_id = ?1 AND season = -1 AND episode = -1",
            params![f.survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((played_b, has_b, changed_b, favorite_b), (0, 0, None, 1));
    let loser_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM playback_units WHERE media_id = ?1",
            params![f.loser.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(loser_rows, 0);
}

fn assert_collections_and_aliases(f: &MergeFixture, db: &Connection) {
    let memberships: Vec<(String, i64)> = {
        let mut stmt = db
            .prepare(
                "SELECT collection_id, sort_order FROM collection_items
                 WHERE media_id = ?1 ORDER BY collection_id",
            )
            .unwrap();
        stmt.query_map(params![f.survivor.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(
        memberships,
        vec![("col1".to_string(), 2), ("col2".to_string(), 1)]
    );

    let (aliases, targets): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), SUM(is_refresh_target) FROM media_aliases WHERE media_id = ?1",
            params![f.survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((aliases, targets), (2, 1));
}

fn assert_metadata_merge(f: &MergeFixture, db: &Connection) {
    let zh = f
        .store
        .get_effective_metadata(f.survivor, "zh-CN")
        .unwrap()
        .expect("survivor zh-CN metadata");
    // R05: effective rows are recomposed from the selected snapshots and overrides;
    // the fixture's snapshots carry no fields, so only override values survive and
    // the losers' stale NFO-composed rows are not carried over.
    assert_eq!(zh.title, "survivor override");
    assert_eq!(zh.overview, None);
    assert_eq!(zh.genres_json, "[]");
    let en = f
        .store
        .get_effective_metadata(f.survivor, "en-US")
        .unwrap()
        .expect("loser-only locale recomposed on the survivor");
    assert_eq!(en.title, "loser en override");
    assert!(
        f.store
            .get_effective_metadata(f.loser, "en-US")
            .unwrap()
            .is_none()
    );

    let sources: Vec<(String, String, String)> = {
        let mut stmt = db
            .prepare(
                "SELECT source, locale, provider_entity_id FROM media_metadata_sources
                 WHERE media_id = ?1 ORDER BY source",
            )
            .unwrap();
        stmt.query_map(params![f.survivor.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(
        sources,
        vec![
            (
                "tmdb".to_string(),
                "zh-CN".to_string(),
                "survivor-snapshot".to_string()
            ),
            (
                "tvdb".to_string(),
                "zh-CN".to_string(),
                "loser-tvdb".to_string()
            ),
        ]
    );
    let overrides: Vec<(String, String)> = {
        let mut stmt = db
            .prepare(
                "SELECT locale, fields_json FROM media_metadata_overrides
                 WHERE media_id = ?1 ORDER BY locale",
            )
            .unwrap();
        stmt.query_map(params![f.survivor.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(
        overrides,
        vec![
            (
                "en-US".to_string(),
                r#"{"title":{"state":"value","value":"loser en override"}}"#.to_string()
            ),
            (
                "zh-CN".to_string(),
                r#"{"title":{"state":"value","value":"survivor override"}}"#.to_string()
            ),
        ]
    );
}

fn assert_subscriptions_and_idempotent_rerun(f: &MergeFixture, db: &Connection) {
    let subscribes = f.store.list_all_subscribes().unwrap();
    assert_eq!(subscribes.len(), 1);
    assert_eq!(subscribes[0].media_id, f.survivor);

    let status = f
        .store
        .advance_media_merge(&f.op, 2000)
        .expect("idempotent advance");
    assert_eq!(status.state, "succeeded");
    let detail = f
        .store
        .library_item_detail(f.library, f.survivor, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(detail.item.file_count, 2);
    let play_count: i64 = db
        .query_row(
            "SELECT play_count FROM playback_units
             WHERE user_id = '00000000-0000-0000-0002-000000000001'
               AND media_id = ?1 AND season = -1 AND episode = -1",
            params![f.survivor.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(play_count, 2);
    let collection_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM collection_items WHERE media_id = ?1",
            params![f.survivor.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(collection_rows, 2);
    let subscribes = f.store.list_all_subscribes().unwrap();
    assert_eq!(subscribes.len(), 1);
    assert_eq!(subscribes[0].media_id, f.survivor);
}

#[test]
fn media_merge_refreshes_survivor_summaries_and_moves_season_metadata() {
    let f = setup_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let user_a = "00000000-0000-0000-0002-000000000001";
    let user_b = "00000000-0000-0000-0002-000000000002";

    assert_eq!(
        read_summary(&db, user_a, f.library, f.survivor),
        Some((1, 1, 1, 30, 10, 1, 1, None, None, None, None, 1)),
        "merged playback facts must produce the survivor summary"
    );
    assert_eq!(
        read_summary(&db, user_b, f.library, f.survivor),
        Some((0, 0, 1, 0, 50, 1, 0, None, None, None, None, 1)),
        "the loser's favourite is visible on the survivor without a new write"
    );

    // `media_seasons` no longer exists (v4), so only the episode rows and the
    // summaries are facts a merge has to move.
    assert_eq!(
        adopted_episodes(&db, f.survivor),
        vec![
            (1, 1, "Survivor E1".to_string()),
            (2, 1, "Loser E2".to_string()),
        ]
    );
    assert!(adopted_episodes(&db, f.loser).is_empty());

    let before_a = read_summary(&db, user_a, f.library, f.survivor);
    let before_b = read_summary(&db, user_b, f.library, f.survivor);
    db.execute(
        "UPDATE media_operations SET state = 'applying', phase_cursor = 'init'
         WHERE operation_id = ?1",
        params![f.op],
    )
    .unwrap();
    let status = f.store.advance_media_merge(&f.op, 3000).unwrap();
    assert_eq!(status.state, "succeeded");
    assert_eq!(read_summary(&db, user_a, f.library, f.survivor), before_a);
    assert_eq!(read_summary(&db, user_b, f.library, f.survivor), before_b);
    assert_eq!(before_a.map(|row| row.11), Some(1));
    assert_eq!(before_b.map(|row| row.11), Some(1));
}

#[test]
fn merge_assertion_rule_deterministic_and_restart_survives() {
    let f = setup_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let user_a = "00000000-0000-0000-0002-000000000001";

    // Deterministic assertion rule check:
    // User A on survivor had played=1, has_assertion=1, clock=10
    // User A on loser had played=0, has_assertion=1, clock=30
    // Result must be: has_assertion=true, played=true, clock=30
    let (has_assertion, played, clock_ms): (i64, i64, Option<i64>) = db
        .query_row(
            "SELECT has_played_assertion, played, played_changed_at_ms FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = -1 AND episode = -1",
            params![user_a, f.survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (has_assertion == 1, played == 1, clock_ms),
        (true, true, Some(30))
    );

    // Season on loser with false@20 is not reversed
    // No assertion on user B -> false / false / NULL
    let user_b = "00000000-0000-0000-0002-000000000002";
    let (has_b, played_b, clock_b): (i64, i64, Option<i64>) = db
        .query_row(
            "SELECT has_played_assertion, played, played_changed_at_ms FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = -1 AND episode = -1",
            params![user_b, f.survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((has_b == 1, played_b == 1, clock_b), (false, false, None));

    // Phase restart / retry does not duplicate counts or recreate instability
    let retry_status = f.store.advance_media_merge(&f.op, 5000).unwrap();
    assert_eq!(retry_status.state, "succeeded");

    let (has_after, played_after, clock_after): (i64, i64, Option<i64>) = db
        .query_row(
            "SELECT has_played_assertion, played, played_changed_at_ms FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = -1 AND episode = -1",
            params![user_a, f.survivor.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (has_after == 1, played_after == 1, clock_after),
        (true, true, Some(30))
    );
}

/// The runtime driver resumes from `unfinished_media_merges()`: a frozen
/// operation is listed, stays listed across a restart, and drops out once the
/// merge succeeds.
#[test]
fn unfinished_merges_are_listed_until_they_succeed_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let survivor = "00000000-0000-0000-0000-0000000000a1";
    let loser = "00000000-0000-0000-0000-0000000000a2";
    create_media(&store, survivor, "Driver Survivor", MediaKind::Tv);
    create_media(&store, loser, "Driver Loser Duplicate", MediaKind::Tv);
    let request = MediaMergeRequest {
        request_id: "00000000-0000-0000-0003-000000000001".to_string(),
        survivor: survivor.parse().unwrap(),
        loser: loser.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), survivor.to_string())]),
    };
    let operation = store.begin_media_merge(&request, 1000).unwrap();
    assert_eq!(
        store.unfinished_media_merges().unwrap(),
        vec![operation.clone()]
    );

    // Restart: the freeze is durable, so the new runtime must resume it.
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    assert_eq!(
        store.unfinished_media_merges().unwrap(),
        vec![operation.clone()]
    );

    let status = store.advance_media_merge(&operation, 2000).unwrap();
    assert_eq!(status.state, "succeeded");
    assert!(store.unfinished_media_merges().unwrap().is_empty());
}

/// A merge carries the loser's NFO/export inputs onto the survivor: the retired
/// identity may not keep rows, the survivor adopts loser-only sidecars, and the
/// survivor's own row wins when both sides observed the same sidecar key.
#[test]
fn merge_moves_the_losers_nfo_inputs_and_keeps_the_survivors_row() {
    let f = setup_pending_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let root: String = db
        .query_row(
            "SELECT root_id FROM library_files WHERE file_id = ?1",
            params![f.loser_file],
            |r| r.get(0),
        )
        .unwrap();
    let (lib, survivor, loser) = (
        f.library.to_string(),
        f.survivor.to_string(),
        f.loser.to_string(),
    );

    // Loser-only sidecar: the survivor must adopt it.
    insert_nfo_input(
        &db,
        &lib,
        &loser,
        &root,
        "/tv/loser/tvshow.nfo",
        "loser-digest",
        3,
    );
    // Both sides observed the same sidecar key, with different exports.
    insert_nfo_input(
        &db,
        &lib,
        &survivor,
        &root,
        "/tv/survivor/tvshow.nfo",
        "survivor-digest",
        4,
    );
    insert_nfo_input(
        &db,
        &lib,
        &loser,
        &root,
        "/tv/survivor/tvshow.nfo",
        "loser-digest",
        5,
    );

    let status = f.store.advance_media_merge(&f.op, 2000).unwrap();
    assert_eq!(status.state, "succeeded");

    let loser_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_metadata_inputs WHERE media_id = ?1",
            params![loser],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(loser_rows, 0, "retired loser kept its NFO inputs");

    let adopted: (String, String, i64) = db
        .query_row(
            "SELECT media_id, export_digest, revision FROM library_metadata_inputs
             WHERE input_key = '/tv/loser/tvshow.nfo'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("loser-only NFO input must survive on the survivor");
    assert_eq!(adopted, (survivor.clone(), "loser-digest".to_string(), 3));

    let contested: Vec<(String, String, i64)> = db
        .prepare(
            "SELECT media_id, export_digest, revision FROM library_metadata_inputs
             WHERE input_key = '/tv/survivor/tvshow.nfo'",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        contested,
        vec![(survivor, "survivor-digest".to_string(), 4)],
        "a contested sidecar key keeps exactly the survivor's own row"
    );
}

/// Media-scoped watch facts are not keyed by file, so they are not carried by the
/// file move: a merge must re-point them onto the survivor itself. The retired
/// identity keeps no timeline markers, no fingerprint season model and no
/// season-comparison history, and the survivor's own value wins per coordinate.
#[test]
fn merge_carries_watch_facts_and_leaves_no_loser_references() {
    let f = setup_pending_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let (survivor, loser) = (f.survivor.to_string(), f.loser.to_string());

    // The survivor already observed S01E01; the loser observed that same episode
    // plus a lone S01E02 that only its (now moved) file knows about.
    insert_marker(&db, &survivor, 1, 1, 10_000, 20_000);
    insert_marker(&db, &loser, 1, 1, 30_000, 40_000);
    insert_marker(&db, &loser, 1, 2, 50_000, 60_000);
    insert_season_model(&db, "model-loser", &loser, 1);
    insert_comparison(&db, &survivor, 1, "survivor-digest");
    insert_comparison(&db, &loser, 1, "loser-digest");

    let status = f.store.advance_media_merge(&f.op, 2000).unwrap();
    assert_eq!(status.state, "succeeded");

    assert_eq!(
        markers(&db, &survivor),
        vec![
            (1, 1, Some(10_000), Some(20_000)),
            (1, 2, Some(50_000), Some(60_000))
        ],
        "survivor keeps its own marker and adopts the loser-only episode"
    );
    assert_eq!(
        markers(&db, &loser),
        Vec::new(),
        "retired loser kept markers"
    );

    let model: (String, i64) = db
        .query_row(
            "SELECT media_id, season FROM fingerprint_season_models WHERE model_id = 'model-loser'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the loser's season model must survive on the survivor");
    assert_eq!(model, (survivor.clone(), 1));

    let (survivor_digest, loser_history) = (comparison(&db, &survivor), comparison(&db, &loser));
    assert_eq!(survivor_digest, "survivor-digest");
    assert_eq!(loser_history, "");

    for table in ["library_items", "library_item_terms", "user_library_items"] {
        assert_eq!(
            rows_for(&db, table, &loser),
            0,
            "retired loser kept {table}"
        );
    }
}

fn insert_marker(db: &Connection, media: &str, season: i64, episode: i64, start: i64, end: i64) {
    db.execute(
        "INSERT INTO media_markers
         (media_id, season, episode, intro_start_ms, intro_end_ms, source, locked, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'detected', 0, 1)",
        params![media, season, episode, start, end],
    )
    .unwrap();
}

fn insert_season_model(db: &Connection, model_id: &str, media: &str, season: i64) {
    db.execute(
        "INSERT INTO fingerprint_season_models
         (model_id, media_id, season, kind, model_version, membership_key, policy_key, model_json,
          created_at_ms)
         VALUES (?1, ?2, ?3, 'intro', 1, 'membership', 'policy', '{}', 1)",
        params![model_id, media, season],
    )
    .unwrap();
}

fn insert_comparison(db: &Connection, media: &str, season: i64, digest: &str) {
    db.execute(
        "INSERT INTO probe_season_comparison_history
         (media_id, season, comparison_digest, published_at_ms) VALUES (?1, ?2, ?3, 1)",
        params![media, season, digest],
    )
    .unwrap();
}

fn markers(db: &Connection, media: &str) -> Vec<(i64, i64, Option<i64>, Option<i64>)> {
    db.prepare(
        "SELECT season, episode, intro_start_ms, intro_end_ms FROM media_markers
         WHERE media_id = ?1 ORDER BY season, episode",
    )
    .unwrap()
    .query_map(params![media], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn comparison(db: &Connection, media: &str) -> String {
    db.query_row(
        "SELECT comparison_digest FROM probe_season_comparison_history WHERE media_id = ?1",
        params![media],
        |r| r.get(0),
    )
    .unwrap_or_default()
}

fn rows_for(db: &Connection, table: &str, media: &str) -> i64 {
    db.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE media_id = ?1"),
        params![media],
        |r| r.get(0),
    )
    .unwrap()
}

/// Identity facts the loser may not keep once the merge is done. History and
/// binding tables (sessions, logs, metrics) deliberately stay bound to the
/// retired id, so they are counted separately.
const LOSER_IDENTITY_TABLES: &[&str] = &[
    "media_aliases",
    "media_metadata",
    "library_metadata_inputs",
    "library_files",
    "collection_items",
    "playback_units",
];

#[derive(Debug, PartialEq)]
struct MergeCounts {
    files: i64,
    aliases: i64,
    inputs: i64,
    markers: i64,
    collections: i64,
    units: i64,
    loser_identity_rows: i64,
    loser_state: String,
    survivor_state: String,
    subscribes_on_survivor: usize,
    subscribes_total: usize,
}

/// The observable end state of a merge, comparable across two fixtures seeded
/// with the same scenario.
fn merge_counts(f: &MergeFixture) -> MergeCounts {
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let (survivor, loser) = (f.survivor.to_string(), f.loser.to_string());
    let subscribes = f.store.list_all_subscribes().unwrap();
    MergeCounts {
        files: rows_for(&db, "library_files", &survivor),
        aliases: rows_for(&db, "media_aliases", &survivor),
        inputs: rows_for(&db, "library_metadata_inputs", &survivor),
        markers: rows_for(&db, "media_markers", &survivor),
        collections: rows_for(&db, "collection_items", &survivor),
        units: rows_for(&db, "playback_units", &survivor),
        loser_identity_rows: LOSER_IDENTITY_TABLES
            .iter()
            .map(|table| rows_for(&db, table, &loser))
            .sum(),
        loser_state: identity_state(&db, &loser),
        survivor_state: identity_state(&db, &survivor),
        subscribes_on_survivor: subscribes
            .iter()
            .filter(|row| row.media_id == f.survivor)
            .count(),
        subscribes_total: subscribes.len(),
    }
}

fn identity_state(db: &Connection, media: &str) -> String {
    db.query_row(
        "SELECT identity_state FROM media WHERE media_id = ?1",
        params![media],
        |r| r.get(0),
    )
    .unwrap_or_default()
}

/// Task 15 step 4: a crash at any persisted phase boundary must not grow the
/// result. Each phase is re-runnable, so rewinding the persisted cursor to that
/// boundary, restarting the process and re-driving the operation has to land on
/// exactly the counts the uninterrupted run produced.
#[test]
fn merge_restart_at_every_persisted_phase_keeps_counts_stable() {
    let reference = setup_merge();
    let expected = merge_counts(&reference);
    assert_eq!(expected.loser_identity_rows, 0, "reference leaves no refs");
    assert_eq!(expected.loser_state, "retired");
    assert_eq!(expected.subscribes_on_survivor, 1);

    for (cursor, state) in [
        ("init", "frozen"),
        ("subscribes", "applying"),
        ("retire", "applying"),
    ] {
        let mut f = setup_merge();
        let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
        db.execute(
            "UPDATE media_operations SET phase_cursor = ?2, state = ?3 WHERE operation_id = ?1",
            params![f.op, cursor, state],
        )
        .unwrap();
        drop(db);
        f.store = Store::open(f.tmp.path()).unwrap();

        assert!(
            f.store.unfinished_media_merges().unwrap().contains(&f.op),
            "{cursor} boundary stays resumable after restart"
        );
        let status = f.store.advance_media_merge(&f.op, 7000).unwrap();
        assert_eq!(status.state, "succeeded", "re-driving from {cursor}");
        let restarted = merge_counts(&f);
        assert_eq!(restarted, expected, "restart from {cursor} changed counts");

        let status = f.store.advance_media_merge(&f.op, 8000).unwrap();
        assert_eq!(status.state, "succeeded");
        assert_eq!(
            merge_counts(&f),
            expected,
            "extra advance after {cursor} grew counts"
        );
    }
}
