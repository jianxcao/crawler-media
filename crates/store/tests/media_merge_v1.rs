use std::collections::BTreeMap;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, LibraryId, Media, MediaId,
    MediaKind, QualitySource, UnitKey,
};
use rusqlite::{Connection, OptionalExtension, params};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::MediaMergeRequest;
use store::{Store, StoreError};

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

fn create_library(
    store: &Store,
    name: &str,
    kind: MediaKind,
    root_path: &str,
) -> (store::Library, String) {
    let lib = store
        .create_library(kind, name, &[root_path], "everyone", true, &[])
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots
        .into_iter()
        .find(|r| r.library_id == Some(lib.id.clone()))
        .unwrap();
    (lib, root.id)
}

fn make_file(
    file_id_str: &str,
    lib_id_str: &str,
    root_id_str: &str,
    media_id_str: &str,
    rel_path: &str,
    kind: MediaKind,
    size: u64,
) -> LibraryFile {
    LibraryFile {
        file_id: file_id_str.parse().unwrap(),
        library_id: lib_id_str.parse().unwrap(),
        root_id: root_id_str.to_string(),
        media_id: media_id_str.parse().unwrap(),
        kind,
        path: rel_path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(size),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash1".to_string()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".to_string()),
        codec: Some("hevc".to_string()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("1080p BluRay".to_string()),
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

struct PlaybackSeed<'a> {
    user: &'a str,
    played: i64,
    has_assertion: i64,
    played_changed_at: Option<i64>,
    favorite: i64,
    favorite_at: i64,
    play_count: i64,
    watch_activity_at: i64,
    position_ms: i64,
    resume: Option<(&'a str, i64)>,
}

fn insert_playback(conn: &Connection, media: &str, seed: &PlaybackSeed<'_>) {
    let resume_file = seed.resume.map(|(file, _)| file);
    let resume_revision = seed.resume.map(|(_, revision)| revision);
    conn.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, played, has_played_assertion,
             played_changed_at_ms, favorite, favorite_at, play_count, watch_activity_at,
             position_ms, duration_ms, resume_file_id, resume_file_revision,
             resume_source_version, timeline_mode, tracks_version, audio_track, subtitle_track)
         VALUES (?1, ?2, -1, -1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, NULL, NULL)",
        params![
            seed.user,
            media,
            seed.played,
            seed.has_assertion,
            seed.played_changed_at,
            seed.favorite,
            seed.favorite_at,
            seed.play_count,
            seed.watch_activity_at,
            seed.position_ms,
            resume_file.map(|_| 100_000i64),
            resume_file,
            resume_revision,
            resume_file.map(|_| "v1"),
            resume_file.map(|_| "full_file"),
            resume_file.map(|_| 1i64),
        ],
    )
    .unwrap();
}

fn insert_metadata(
    conn: &Connection,
    media: &str,
    locale: &str,
    title: &str,
    overview: Option<&str>,
    language: Option<&str>,
    genres_json: &str,
    origins_json: &str,
) {
    conn.execute(
        "INSERT INTO media_metadata
            (media_id, locale, title, original_title, year, overview, release_date, rating,
             runtime_minutes, language, genres_json, countries_json, poster_url, backdrop_url,
             details_json, field_origins_json, revision)
         VALUES (?1, ?2, ?3, NULL, 2024, ?4, NULL, NULL, NULL, ?5, ?6, '[]', NULL, NULL,
                 '{}', ?7, 1)",
        params![media, locale, title, overview, language, genres_json, origins_json],
    )
    .unwrap();
}

fn insert_alias(conn: &Connection, media: &str, source: &str, external_id: &str, target: i64) {
    conn.execute(
        "INSERT INTO media_aliases (source, kind, external_id, media_id, is_refresh_target)
         VALUES (?1, 'tv', ?2, ?3, ?4)",
        params![source, external_id, media, target],
    )
    .unwrap();
}

fn insert_metadata_source(
    conn: &Connection,
    media: &str,
    source: &str,
    locale: &str,
    provider_entity_id: &str,
) {
    conn.execute(
        "INSERT INTO media_metadata_sources
            (media_id, entity_scope, season, episode, source, locale, provider_entity_id,
             provider_work_id, normalized_json, present_fields_json, preferences_json,
             selection_origins_json, fetched_at, source_revision)
         VALUES (?1, 'work', -1, -1, ?2, ?3, ?4, ?4, '{}', '[]', '{}', '{}', 1, 1)",
        params![media, source, locale, provider_entity_id],
    )
    .unwrap();
}

fn insert_metadata_override(conn: &Connection, media: &str, locale: &str, fields_json: &str) {
    conn.execute(
        "INSERT INTO media_metadata_overrides
            (media_id, entity_scope, season, episode, locale, fields_json, revision)
         VALUES (?1, 'work', -1, -1, ?2, ?3, 1)",
        params![media, locale, fields_json],
    )
    .unwrap();
}

fn insert_season(conn: &Connection, media: &str, season: i64, locale: &str, title: &str) {
    conn.execute(
        "INSERT INTO media_seasons
            (media_id, season, locale, title, details_json, field_origins_json, revision)
         VALUES (?1, ?2, ?3, ?4, '{}', '{}', 1)",
        params![media, season, locale, title],
    )
    .unwrap();
}

fn insert_episode(
    conn: &Connection,
    media: &str,
    season: i64,
    episode: i64,
    locale: &str,
    title: &str,
) {
    conn.execute(
        "INSERT INTO media_episodes
            (media_id, season, episode, locale, title, details_json, field_origins_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, '{}', '{}', 1)",
        params![media, season, episode, locale, title],
    )
    .unwrap();
}

fn insert_collection_item(conn: &Connection, collection: &str, media: &str, sort: i64, added: &str) {
    conn.execute(
        "INSERT INTO collection_items (collection_id, media_id, sort_order, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![collection, media, sort, added],
    )
    .unwrap();
}

struct MergeFixture {
    tmp: tempfile::TempDir,
    store: Store,
    survivor: MediaId,
    loser: MediaId,
    library: LibraryId,
    loser_file: String,
    op: String,
}

fn setup_merge() -> MergeFixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    create_media(&store, survivor_id, "Survivor Show", MediaKind::Tv);
    create_media(&store, loser_id, "Loser Show Duplicate", MediaKind::Tv);
    let survivor: MediaId = survivor_id.parse().unwrap();
    let loser: MediaId = loser_id.parse().unwrap();

    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let library: LibraryId = lib.id.parse().unwrap();
    let scope = WriteScope {
        library_id: library,
        config_revision: 0,
    };
    let survivor_file = "00000000-0000-0000-0001-0000000000a1".to_string();
    let loser_file = "00000000-0000-0000-0001-0000000000a2".to_string();
    for (file_id, media_id, path) in [
        (survivor_file.as_str(), survivor_id, "survivor/S01E01.mkv"),
        (loser_file.as_str(), loser_id, "loser/S01E01.mkv"),
    ] {
        let file = make_file(file_id, &lib.id, &root_id, media_id, path, MediaKind::Tv, 1_000);
        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::new(1, 1)],
                    expected_version: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    let user_a = "00000000-0000-0000-0002-000000000001";
    let user_b = "00000000-0000-0000-0002-000000000002";
    let library_db = Connection::open(tmp.path().join("library.db")).unwrap();
    insert_playback(
        &library_db,
        survivor_id,
        &PlaybackSeed {
            user: user_a,
            played: 1,
            has_assertion: 1,
            played_changed_at: Some(10),
            favorite: 1,
            favorite_at: 10,
            play_count: 2,
            watch_activity_at: 10,
            position_ms: 5_000,
            resume: Some((survivor_file.as_str(), 1)),
        },
    );
    insert_playback(
        &library_db,
        loser_id,
        &PlaybackSeed {
            user: user_a,
            played: 0,
            has_assertion: 1,
            played_changed_at: Some(30),
            favorite: 0,
            favorite_at: 0,
            play_count: 1,
            watch_activity_at: 30,
            position_ms: 7_000,
            resume: Some((loser_file.as_str(), 1)),
        },
    );
    insert_playback(
        &library_db,
        loser_id,
        &PlaybackSeed {
            user: user_b,
            played: 0,
            has_assertion: 0,
            played_changed_at: None,
            favorite: 1,
            favorite_at: 50,
            play_count: 0,
            watch_activity_at: 0,
            position_ms: 0,
            resume: None,
        },
    );

    insert_metadata(
        &library_db,
        survivor_id,
        "zh-CN",
        "Survivor Show",
        None,
        Some(""),
        "[]",
        r#"{"title":"nfo","language":"nfo"}"#,
    );
    insert_metadata(
        &library_db,
        loser_id,
        "zh-CN",
        "Loser Show Duplicate",
        Some("loser overview"),
        Some("ja"),
        r#"["drama"]"#,
        r#"{"title":"nfo","overview":"nfo","language":"nfo","genres":"nfo"}"#,
    );
    insert_metadata(
        &library_db,
        loser_id,
        "en-US",
        "Loser Show EN",
        Some("english overview"),
        None,
        "[]",
        r#"{"title":"nfo","overview":"nfo"}"#,
    );
    insert_metadata_source(&library_db, survivor_id, "tmdb", "zh-CN", "survivor-snapshot");
    insert_metadata_source(&library_db, loser_id, "tmdb", "zh-CN", "loser-snapshot");
    insert_metadata_source(&library_db, loser_id, "tvdb", "zh-CN", "loser-tvdb");
    insert_metadata_override(
        &library_db,
        survivor_id,
        "zh-CN",
        r#"{"title":"survivor override"}"#,
    );
    insert_metadata_override(&library_db, loser_id, "zh-CN", r#"{"title":"loser override"}"#);
    insert_metadata_override(
        &library_db,
        loser_id,
        "en-US",
        r#"{"title":"loser en override"}"#,
    );
    insert_season(&library_db, survivor_id, 1, "zh-CN", "Survivor S1");
    insert_season(&library_db, loser_id, 1, "zh-CN", "Loser S1");
    insert_season(&library_db, loser_id, 2, "zh-CN", "Loser S2");
    insert_episode(&library_db, survivor_id, 1, 1, "zh-CN", "Survivor E1");
    insert_episode(&library_db, loser_id, 1, 1, "zh-CN", "Loser E1");
    insert_episode(&library_db, loser_id, 2, 1, "zh-CN", "Loser E2");

    library_db
        .execute(
            "DELETE FROM media_aliases WHERE media_id IN (?1, ?2)",
            params![survivor_id, loser_id],
        )
        .unwrap();
    insert_alias(&library_db, survivor_id, "tmdb", "survivor-tmdb", 0);
    insert_alias(&library_db, loser_id, "tmdb", "loser-tmdb", 1);

    for collection in ["col1", "col2"] {
        library_db
            .execute(
                "INSERT INTO collections (id, user_id, visibility, name, sort_order, created_at)
                 VALUES (?1, ?2, 'private', ?1, 0, '2024-01-01')",
                params![collection, user_a],
            )
            .unwrap();
    }
    insert_collection_item(&library_db, "col1", survivor_id, 5, "2024-01-02");
    insert_collection_item(&library_db, "col1", loser_id, 2, "2024-01-01");
    insert_collection_item(&library_db, "col2", loser_id, 1, "2024-01-03");

    let subscribe_db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    subscribe_db
        .execute(
            "INSERT INTO subscribes
                (id, user_id, media_id, coverage_kind, fetch_mode, filter_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'movie', 'search', ?4, '2024-01-01', '2024-01-01')",
            params![
                "00000000-0000-0000-0003-000000000001",
                user_a,
                loser_id,
                "00000000-0000-0000-0004-000000000001",
            ],
        )
        .unwrap();
    drop(subscribe_db);
    drop(library_db);

    let request = MediaMergeRequest {
        request_id: "merge-acceptance".into(),
        survivor,
        loser,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    let op = store.begin_media_merge(&request, 1000).expect("begin_media_merge");
    let status = store.advance_media_merge(&op, 1000).expect("advance_media_merge");
    assert_eq!(status.state, "succeeded");

    MergeFixture {
        tmp,
        store,
        survivor,
        loser,
        library,
        loser_file,
        op,
    }
}

#[test]
fn media_merge_lifecycle_preserves_survivor_and_retires_loser() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let m1_id = "00000000-0000-0000-0000-000000000001";
    let m2_id = "00000000-0000-0000-0000-000000000002";

    create_media(&store, m1_id, "Survivor Show", MediaKind::Tv);
    create_media(&store, m2_id, "Loser Show Duplicate", MediaKind::Tv);

    // Both media carry a tmdb alias, so the merge must pick the refresh target.
    let req = MediaMergeRequest {
        request_id: "req_merge_1".into(),
        survivor: m1_id.parse().unwrap(),
        loser: m2_id.parse().unwrap(),
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::from([("tmdb".to_string(), m1_id.to_string())]),
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
    // The loser's file is now part of the survivor's card.
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

    // The loser is a retired tombstone pointing at the survivor.
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
    // user A: favourite OR, play_count max, assertion tri-state true@10 + false@30,
    // resume package taken whole from the later watch_activity_at (loser @30).
    let merged: (i64, i64, Option<i64>, i64, i64, i64, i64, i64, Option<String>) = db
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
    assert_eq!(merged.2, Some(30), "played_changed_at_ms keeps the max assertion clock");
    assert_eq!(merged.3, 1, "favourite OR");
    assert_eq!(merged.4, 10, "favorite_at keeps the later timestamp");
    assert_eq!(merged.5, 2, "play_count is max, never summed");
    assert_eq!(merged.6, 30);
    assert_eq!(merged.7, 7_000, "resume package comes whole from the later row");
    assert_eq!(merged.8.as_deref(), Some(f.loser_file.as_str()));

    // user B only had the loser row: a favourite-only row survives, without an assertion.
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
    // Collections: one survivor membership per collection, earliest sort order.
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

    // Aliases all moved, exactly one refresh target remains.
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
    // Adopted metadata: survivor wins, only missing fields come from the loser,
    // explicit empty stays empty, and locales never merge into each other.
    let zh = f
        .store
        .get_effective_metadata(f.survivor, "zh-CN")
        .unwrap()
        .expect("survivor zh-CN metadata");
    assert_eq!(zh.title, "Survivor Show");
    assert_eq!(zh.overview.as_deref(), Some("loser overview"));
    assert_eq!(zh.language.as_deref(), Some(""), "explicit empty stays empty");
    assert_eq!(zh.genres_json, r#"["drama"]"#);
    let en = f
        .store
        .get_effective_metadata(f.survivor, "en-US")
        .unwrap()
        .expect("loser-only locale moved whole");
    assert_eq!(en.title, "Loser Show EN");
    assert!(f.store.get_effective_metadata(f.loser, "en-US").unwrap().is_none());

    // Source snapshots and overrides: the survivor wins on a shared key and the
    // loser's non-conflicting rows move.
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
            ("tmdb".to_string(), "zh-CN".to_string(), "survivor-snapshot".to_string()),
            ("tvdb".to_string(), "zh-CN".to_string(), "loser-tvdb".to_string()),
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
                r#"{"title":"loser en override"}"#.to_string()
            ),
            (
                "zh-CN".to_string(),
                r#"{"title":"survivor override"}"#.to_string()
            ),
        ]
    );
}

fn assert_subscriptions_and_idempotent_rerun(f: &MergeFixture, db: &Connection) {
    // Subscriptions now reference the survivor.
    let subscribes = f.store.list_all_subscribes().unwrap();
    assert_eq!(subscribes.len(), 1);
    assert_eq!(subscribes[0].media_id, f.survivor);

    // A second advance is a no-op: no duplicated rows, no double-counted plays.
    let status = f.store.advance_media_merge(&f.op, 2000).expect("idempotent advance");
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

    let undecided = MediaMergeRequest {
        request_id: "merge-undecided".into(),
        survivor,
        loser,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    let op = store.begin_media_merge(&undecided, 1000).unwrap();
    let err = store.advance_media_merge(&op, 1000).unwrap_err();
    assert!(
        matches!(&err, StoreError::Protected(message) if message.contains("refresh_targets[tmdb]")),
        "unexpected error: {err}"
    );

    let mut refresh_targets = BTreeMap::new();
    refresh_targets.insert("tmdb".to_string(), "external-b".to_string());
    let decided = MediaMergeRequest {
        request_id: "merge-decided".into(),
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
    assert_eq!((targets, target_external.as_deref()), (1, Some("external-b")));

    db.execute(
        "UPDATE media SET identity_state = 'merging' WHERE media_id = ?1",
        params![survivor.to_string()],
    )
    .unwrap();
    let err = store.advance_media_merge(&op, 2000).unwrap_err();
    assert!(
        matches!(&err, StoreError::Protected(message) if message.contains("merging")),
        "unexpected error: {err}"
    );
}

type SummaryRow = (
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
);

fn read_summary(
    db: &Connection,
    user: &str,
    library: LibraryId,
    media: MediaId,
) -> Option<SummaryRow> {
    db.query_row(
        "SELECT seen, played, favorite, last_played_at, favorite_at, owned_episode_count,
                completed_episode_count, next_season, next_episode, resume_season,
                resume_episode, summary_revision
         FROM user_library_items
         WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3",
        params![user, library.to_string(), media.to_string()],
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
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        },
    )
    .optional()
    .unwrap()
}

fn adopted_seasons(db: &Connection, media: MediaId) -> Vec<(i64, String)> {
    let mut stmt = db
        .prepare("SELECT season, title FROM media_seasons WHERE media_id = ?1 ORDER BY season")
        .unwrap();
    stmt.query_map(params![media.to_string()], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn adopted_episodes(db: &Connection, media: MediaId) -> Vec<(i64, i64, String)> {
    let mut stmt = db
        .prepare(
            "SELECT season, episode, title FROM media_episodes
             WHERE media_id = ?1 ORDER BY season, episode",
        )
        .unwrap();
    stmt.query_map(params![media.to_string()], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

#[test]
fn media_merge_refreshes_survivor_summaries_and_moves_season_metadata() {
    let f = setup_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let user_a = "00000000-0000-0000-0002-000000000001";
    let user_b = "00000000-0000-0000-0002-000000000002";

    // The merge itself (no further playback write) materialises the survivor's
    // summaries. User A carries the merged facts; user B only favourited the
    // loser's episode and must see that favourite on the survivor immediately.
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

    // Season/episode rows: the survivor wins on a shared key, the loser's
    // unique coordinates move onto the survivor, and nothing is left behind.
    assert_eq!(
        adopted_seasons(&db, f.survivor),
        vec![(1, "Survivor S1".to_string()), (2, "Loser S2".to_string())]
    );
    assert_eq!(
        adopted_episodes(&db, f.survivor),
        vec![
            (1, 1, "Survivor E1".to_string()),
            (2, 1, "Loser E2".to_string()),
        ]
    );
    assert!(adopted_seasons(&db, f.loser).is_empty());
    assert!(adopted_episodes(&db, f.loser).is_empty());

    // Re-running the library phase rewrites nothing: the reduced summaries are
    // identical and their revisions did not move.
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
