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
    assert_eq!(
        store.advance_media_merge(&op, 3000).unwrap().state,
        "succeeded"
    );
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
    let op = store
        .begin_media_merge(&req("00000000-0000-0000-0001-0000000000b1"), 1000)
        .unwrap();
    assert!(store.advance_media_merge(&op, 1100).is_err());
    let failed_operation_is_recoverable =
        store.unfinished_media_merges().unwrap() == vec![op.clone()];
    assert!(failed_operation_is_recoverable);
    assert!(
        store
            .begin_media_merge(&req("00000000-0000-0000-0001-0000000000b2"), 1200)
            .is_err(),
        "a newer operation must not take over the failed one's freeze"
    );
    db.execute_batch("DROP TRIGGER fail_merge;").unwrap();
    assert_eq!(
        store.advance_media_merge(&op, 1300).unwrap().state,
        "succeeded"
    );
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
    override_row(&db, SURVIVOR, r#"{"title":{"state":"value","value":"S"}}"#);
    override_row(
        &db,
        LOSER,
        r#"{"title":{"state":"value","value":"L"},"overview":{"state":"value","value":"LO"}}"#,
    );
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
    assert!(
        store.unfinished_media_merges().unwrap().is_empty(),
        "zero writes"
    );

    db.execute(
        "UPDATE media_metadata_overrides SET fields_json = '{\"overview\":{\"state\":\"value\",\"value\":\"LO\"}}' WHERE media_id = ?1",
        params![LOSER],
    )
    .unwrap();
    let op = store.begin_media_merge(&req, 1100).unwrap();
    assert_eq!(
        store.advance_media_merge(&op, 1200).unwrap().state,
        "succeeded"
    );
    let merged: String = db
        .query_row(
            "SELECT fields_json FROM media_metadata_overrides WHERE media_id = ?1",
            params![SURVIVOR],
            |r| r.get(0),
        )
        .unwrap();
    let merged: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(
        merged,
        serde_json::json!({"title": {"state": "value", "value": "S"}, "overview": {"state": "value", "value": "LO"}})
    );
}

fn scoped_snapshot(
    db: &Connection,
    media: &str,
    (scope, season, episode): (&str, i64, i64),
    entity: &str,
    work: &str,
    fields: &domain::MetadataFields,
) {
    db.execute(
        "INSERT INTO media_metadata_sources
            (media_id, entity_scope, season, episode, source, locale, provider_entity_id,
             provider_work_id, normalized_json, present_fields_json, preferences_json,
             selection_origins_json, fetched_at, source_revision)
         VALUES (?1, ?2, ?3, ?4, 'tmdb', 'zh-CN', ?5, ?6, ?7, '[]', '{}', '{}', 100, 1)",
        params![
            media,
            scope,
            season,
            episode,
            entity,
            work,
            serde_json::to_string(fields).unwrap()
        ],
    )
    .unwrap();
}

fn fields(title: Option<&str>, tagline: Option<&str>) -> domain::MetadataFields {
    domain::MetadataFields {
        title: title.map_or(domain::FieldValue::Missing, |t| {
            domain::FieldValue::Value(t.into())
        }),
        tagline: tagline.map_or(domain::FieldValue::Missing, |t| {
            domain::FieldValue::Value(t.into())
        }),
        ..Default::default()
    }
}

/// Two TV Media claiming different TMDB works (p2 survivor, p3 loser), merged with
/// the loser's p3 chosen as refresh target.
fn cross_entity_fixture() -> (tempfile::TempDir, Store, Connection, MediaMergeRequest) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    create_media(&store, SURVIVOR, "Survivor", MediaKind::Tv);
    create_media(&store, LOSER, "Loser", MediaKind::Tv);
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute("DELETE FROM media_aliases", []).unwrap();
    merge_support::insert_alias(&db, SURVIVOR, "tmdb", "p2", 1);
    merge_support::insert_alias(&db, LOSER, "tmdb", "p3", 1);
    let req = MediaMergeRequest {
        request_id: "00000000-0000-0000-0001-0000000000b1".into(),
        survivor: SURVIVOR.parse().unwrap(),
        loser: LOSER.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), "p3".to_string())]),
    };
    (tmp, store, db, req)
}

/// R21: cross-entity merge — the chosen target's snapshot wins at work/episode
/// scope even when older; the superseded non-target fields the target lacks are a
/// per-field pending fallback; a non-target-only episode is served whole as pending.
#[test]
fn cross_entity_target_wins_and_non_target_fields_are_pending_fallback() {
    let (tmp, store, db, req) = cross_entity_fixture();
    let work = ("work", -1, -1);
    scoped_snapshot(
        &db,
        SURVIVOR,
        work,
        "p2",
        "p2",
        &fields(Some("P2"), Some("P2 TAG")),
    );
    scoped_snapshot(&db, LOSER, work, "p3", "p3", &fields(Some("P3"), None));
    scoped_snapshot(
        &db,
        SURVIVOR,
        ("episode", 1, 1),
        "p2e1",
        "p2",
        &fields(Some("P2 E1"), None),
    );
    scoped_snapshot(
        &db,
        LOSER,
        ("episode", 1, 2),
        "p3e2",
        "p3",
        &fields(Some("P3 E2"), None),
    );

    let op = store.begin_media_merge(&req, 3000).unwrap();
    assert_eq!(
        store.advance_media_merge(&op, 3000).unwrap().state,
        "succeeded"
    );

    let (library, _) = merge_support::create_library(
        &store,
        "TV",
        MediaKind::Tv,
        tmp.path().join("tv").to_str().unwrap(),
    );
    let library: domain::LibraryId = library.id.parse().unwrap();
    let media: domain::MediaId = SURVIVOR.parse().unwrap();
    let composed = store
        .effective_metadata_fields(library, media, domain::EntityScope::Work, "zh-CN")
        .unwrap();
    assert_eq!(
        composed.fields.title.value().map(String::as_str),
        Some("P3")
    );
    assert!(!composed.field_origins["title"].pending_target_fallback);
    assert_eq!(
        composed.fields.tagline.value().map(String::as_str),
        Some("P2 TAG"),
        "a field the target lacks falls back per field"
    );
    let tag = &composed.field_origins["tagline"];
    assert!(tag.pending_target_fallback);
    assert_eq!(tag.target_external_id.as_deref(), Some("p3"));
    assert_eq!(tag.provider_entity_id, "p2");

    let e1 = store
        .effective_metadata_fields(
            library,
            media,
            domain::EntityScope::Episode {
                season: 1,
                episode: 1,
            },
            "zh-CN",
        )
        .unwrap();
    assert_eq!(e1.fields.title.value().map(String::as_str), Some("P2 E1"));
    assert!(e1.field_origins["title"].pending_target_fallback);
    let e2 = store
        .effective_metadata_fields(
            library,
            media,
            domain::EntityScope::Episode {
                season: 1,
                episode: 2,
            },
            "zh-CN",
        )
        .unwrap();
    assert_eq!(e2.fields.title.value().map(String::as_str), Some("P3 E2"));
    assert!(!e2.field_origins["title"].pending_target_fallback);
}

/// R21: the target side must describe the target at season/episode scope too.
#[test]
fn cross_entity_target_with_foreign_episode_snapshot_is_refused() {
    let (_tmp, store, db, req) = cross_entity_fixture();
    scoped_snapshot(
        &db,
        LOSER,
        ("episode", 1, 1),
        "p9e1",
        "p9",
        &fields(Some("X"), None),
    );
    let error = store.begin_media_merge(&req, 3000).unwrap_err();
    assert!(error.to_string().contains("p9"), "{error}");
    let state: String = db
        .query_row(
            "SELECT identity_state FROM media WHERE media_id = ?1",
            params![LOSER],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(state, "merging", "a refused merge writes nothing");
}

fn adopt(store: &Store, media: &str, scope: domain::EntityScope, title: &str, fetched_at: i64) {
    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value(title.into());
    fields.overview = if title.ends_with('B') {
        domain::FieldValue::Empty
    } else {
        domain::FieldValue::Value(format!("{title} overview"))
    };
    let media_id: domain::MediaId = media.parse().unwrap();
    let entity = match scope {
        domain::EntityScope::Work => media.to_string(),
        domain::EntityScope::Season(s) => format!("{media}:{s}"),
        domain::EntityScope::Episode { season, episode } => format!("{media}:{season}:{episode}"),
    };
    store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id,
                scope,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: entity,
                provider_work_id: media.to_string(),
                fetched_at,
                normalized: domain::NormalizedMetadata {
                    fields,
                    field_origins: Default::default(),
                },
                expected_revision: Some(store.media_revision(media_id).unwrap().unwrap()),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec![],
                },
            },
            fetched_at,
        )
        .unwrap();
}

/// R05: the merge picks the loser's snapshot (refresh target) and the persisted effective
/// rows — work, season and episode — are recomposed from it, not kept from the survivor.
#[test]
fn merge_recomposes_effective_rows_from_selected_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    create_media(&store, SURVIVOR, "Survivor", MediaKind::Tv);
    create_media(&store, LOSER, "Loser", MediaKind::Tv);
    let episode = domain::EntityScope::Episode {
        season: 1,
        episode: 2,
    };
    for scope in [
        domain::EntityScope::Work,
        domain::EntityScope::Season(1),
        episode,
    ] {
        adopt(&store, SURVIVOR, scope, "Adopted A", 1000);
        adopt(&store, LOSER, scope, "Adopted B", 2000);
    }
    let req = MediaMergeRequest {
        request_id: "00000000-0000-0000-0001-0000000000d1".into(),
        survivor: SURVIVOR.parse().unwrap(),
        loser: LOSER.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), LOSER.to_string())]),
    };
    let op = store.begin_media_merge(&req, 3000).unwrap();
    assert_eq!(
        store.advance_media_merge(&op, 3000).unwrap().state,
        "succeeded"
    );
    let survivor: domain::MediaId = SURVIVOR.parse().unwrap();
    let work = store
        .get_effective_metadata(survivor, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(work.title, "Adopted B");
    assert!(
        work.overview.is_none(),
        "explicit empty from the selected snapshot stays empty"
    );
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let media_title: String = db
        .query_row(
            "SELECT title FROM media WHERE media_id = ?1",
            params![SURVIVOR],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(media_title, "Adopted B");
    let season: String = db
        .query_row(
            "SELECT title FROM media_seasons WHERE media_id = ?1 AND season = 1",
            params![SURVIVOR],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(season, "Adopted B");
    let episode_title: String = db
        .query_row(
            "SELECT title FROM media_episodes WHERE media_id = ?1 AND season = 1 AND episode = 2",
            params![SURVIVOR],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(episode_title, "Adopted B");
    let origins_before = work.field_origins_json.clone();
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reopened.advance_media_merge(&op, 4000).unwrap().state,
        "succeeded"
    );
    let after = reopened
        .get_effective_metadata(survivor, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(
        after.field_origins_json, origins_before,
        "repeat advance is idempotent"
    );
}
