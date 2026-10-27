#![allow(dead_code)]

use std::collections::BTreeMap;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, LibraryId, Media, MediaId,
    MediaKind, QualitySource, UnitKey,
};
use rusqlite::{Connection, OptionalExtension, params};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::MediaMergeRequest;

pub fn create_media(store: &Store, id: &str, title: &str, kind: MediaKind) -> Media {
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

pub fn create_library(
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
        .find(|r| r.library_id == lib.id.clone())
        .unwrap();
    (lib, root.id)
}

pub fn make_file(
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

pub struct PlaybackSeed<'a> {
    pub user: &'a str,
    pub played: i64,
    pub has_assertion: i64,
    pub played_changed_at: Option<i64>,
    pub favorite: i64,
    pub favorite_at: i64,
    pub play_count: i64,
    pub watch_activity_at: i64,
    pub position_ms: i64,
    pub resume: Option<(&'a str, i64)>,
}

pub fn insert_playback(conn: &Connection, media: &str, seed: &PlaybackSeed<'_>) {
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

#[allow(clippy::too_many_arguments)]
pub fn insert_metadata(
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
        params![
            media,
            locale,
            title,
            overview,
            language,
            genres_json,
            origins_json
        ],
    )
    .unwrap();
}

/// Register a work-scope NFO sidecar input, the way a scan does when it observes
/// `tvshow.nfo` next to an episode. `input_key`/`path_key` are the canonical
/// sidecar paths, so the row survives only while that directory still publishes a
/// file of the owning card.
pub fn insert_nfo_input(
    conn: &Connection,
    library: &str,
    media: &str,
    root_id: &str,
    input_key: &str,
    export_digest: &str,
    revision: i64,
) {
    conn.execute(
        "INSERT INTO library_metadata_inputs
            (library_id, media_id, entity_scope, season, episode, locale, input_key, root_id,
             path_key, parse_state, source_version, fields_json, present_fields_json,
             provenance_json, export_id, export_digest, export_fields_json, revision)
         VALUES (?1, ?2, 'work', -1, -1, 'zh-CN', ?3, ?4, ?3, 'valid', 'nfo_v1', '{}', '[]',
                 '{}', ?5, ?6, '{}', ?7)",
        params![
            library,
            media,
            input_key,
            root_id,
            format!("export-{}", input_key.replace('/', "-")),
            export_digest,
            revision,
        ],
    )
    .unwrap();
}

pub fn insert_alias(conn: &Connection, media: &str, source: &str, external_id: &str, target: i64) {
    conn.execute(
        "INSERT INTO media_aliases (source, kind, external_id, media_id, is_refresh_target)
         VALUES (?1, 'tv', ?2, ?3, ?4)",
        params![source, external_id, media, target],
    )
    .unwrap();
}

pub fn insert_metadata_source(
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

pub fn insert_metadata_override(conn: &Connection, media: &str, locale: &str, fields_json: &str) {
    conn.execute(
        "INSERT INTO media_metadata_overrides
            (media_id, entity_scope, season, episode, locale, fields_json, revision)
         VALUES (?1, 'work', -1, -1, ?2, ?3, 1)",
        params![media, locale, fields_json],
    )
    .unwrap();
}

pub fn insert_episode(
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

pub fn insert_collection_item(
    conn: &Connection,
    collection: &str,
    media: &str,
    sort: i64,
    added: &str,
) {
    conn.execute(
        "INSERT INTO collection_items (collection_id, media_id, sort_order, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![collection, media, sort, added],
    )
    .unwrap();
}

pub struct MergeFixture {
    pub tmp: tempfile::TempDir,
    pub store: Store,
    pub survivor: MediaId,
    pub loser: MediaId,
    pub library: LibraryId,
    pub loser_file: String,
    pub op: String,
}

pub fn setup_merge() -> MergeFixture {
    setup_merge_inner(true)
}

/// The same scenario as [`setup_merge`], but left frozen: the caller seeds the
/// rows a phase must carry and then drives the phases itself.
pub fn setup_pending_merge() -> MergeFixture {
    setup_merge_inner(false)
}

fn setup_merge_inner(advance: bool) -> MergeFixture {
    let (tmp, store, library) = seed_merge_files();
    let library_db = Connection::open(tmp.path().join("library.db")).unwrap();
    seed_merge_playback(&library_db);
    seed_merge_metadata(&library_db);
    seed_merge_relations(&library_db);
    seed_merge_subscribe(tmp.path());
    drop(library_db);
    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    let survivor = survivor_id.parse().unwrap();
    let loser = loser_id.parse().unwrap();
    let canonical_req_id = "00000000-0000-0000-0005-000000000001".to_string();
    let request = MediaMergeRequest {
        request_id: canonical_req_id,
        survivor,
        loser,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: BTreeMap::new(),
    };
    let op = store
        .begin_media_merge(&request, 1000)
        .expect("begin_media_merge");
    if advance {
        let status = store
            .advance_media_merge(&op, 1000)
            .expect("advance_media_merge");
        assert_eq!(status.state, "succeeded");
    }

    MergeFixture {
        tmp,
        store,
        survivor,
        loser,
        library,
        loser_file: "00000000-0000-0000-0001-0000000000a2".into(),
        op,
    }
}

fn seed_merge_files() -> (tempfile::TempDir, Store, LibraryId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    create_media(&store, survivor_id, "Survivor Show", MediaKind::Tv);
    create_media(&store, loser_id, "Loser Show Duplicate", MediaKind::Tv);

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
        let file = make_file(
            file_id,
            &lib.id,
            &root_id,
            media_id,
            path,
            MediaKind::Tv,
            1_000,
        );
        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::new(1, 1)],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    (tmp, store, library)
}

fn seed_merge_playback(library_db: &Connection) {
    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    let survivor_file = "00000000-0000-0000-0001-0000000000a1".to_string();
    let loser_file = "00000000-0000-0000-0001-0000000000a2".to_string();
    let user_a = "00000000-0000-0000-0002-000000000001";
    let user_b = "00000000-0000-0000-0002-000000000002";
    insert_playback(
        library_db,
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
        library_db,
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
        library_db,
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
}

fn seed_merge_metadata(library_db: &Connection) {
    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    insert_metadata(
        library_db,
        survivor_id,
        "zh-CN",
        "Survivor Show",
        None,
        Some(""),
        "[]",
        r#"{"title":"nfo","language":"nfo"}"#,
    );
    insert_metadata(
        library_db,
        loser_id,
        "zh-CN",
        "Loser Show Duplicate",
        Some("loser overview"),
        Some("ja"),
        r#"["drama"]"#,
        r#"{"title":"nfo","overview":"nfo","language":"nfo","genres":"nfo"}"#,
    );
    insert_metadata(
        library_db,
        loser_id,
        "en-US",
        "Loser Show EN",
        Some("english overview"),
        None,
        "[]",
        r#"{"title":"nfo","overview":"nfo"}"#,
    );
    insert_metadata_source(
        library_db,
        survivor_id,
        "tmdb",
        "zh-CN",
        "survivor-snapshot",
    );
    insert_metadata_source(library_db, loser_id, "tmdb", "zh-CN", "loser-snapshot");
    insert_metadata_source(library_db, loser_id, "tvdb", "zh-CN", "loser-tvdb");
    insert_metadata_override(
        library_db,
        survivor_id,
        "zh-CN",
        r#"{"title":{"state":"value","value":"survivor override"}}"#,
    );
    insert_metadata_override(
        library_db,
        loser_id,
        "zh-CN",
        // Same lock as the survivor: a conflicting lock is refused (R21), not dropped.
        r#"{"title":{"state":"value","value":"survivor override"}}"#,
    );
    insert_metadata_override(
        library_db,
        loser_id,
        "en-US",
        r#"{"title":{"state":"value","value":"loser en override"}}"#,
    );
    insert_episode(library_db, survivor_id, 1, 1, "zh-CN", "Survivor E1");
    insert_episode(library_db, loser_id, 1, 1, "zh-CN", "Loser E1");
    insert_episode(library_db, loser_id, 2, 1, "zh-CN", "Loser E2");
}

fn seed_merge_relations(library_db: &Connection) {
    let survivor_id = "00000000-0000-0000-0000-0000000000a1";
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    let user_a = "00000000-0000-0000-0002-000000000001";
    library_db
        .execute(
            "DELETE FROM media_aliases WHERE media_id IN (?1, ?2)",
            params![survivor_id, loser_id],
        )
        .unwrap();
    insert_alias(library_db, survivor_id, "tmdb", "survivor-tmdb", 0);
    insert_alias(library_db, loser_id, "tmdb", "loser-tmdb", 1);

    for collection in ["col1", "col2"] {
        library_db
            .execute(
                "INSERT INTO collections (id, user_id, visibility, name, sort_order, created_at)
                 VALUES (?1, ?2, 'private', ?1, 0, '2024-01-01')",
                params![collection, user_a],
            )
            .unwrap();
    }
    insert_collection_item(library_db, "col1", survivor_id, 5, "2024-01-02");
    insert_collection_item(library_db, "col1", loser_id, 2, "2024-01-01");
    insert_collection_item(library_db, "col2", loser_id, 1, "2024-01-03");
}

fn seed_merge_subscribe(data_dir: &std::path::Path) {
    let loser_id = "00000000-0000-0000-0000-0000000000a2";
    let user_a = "00000000-0000-0000-0002-000000000001";
    let subscribe_db = Connection::open(data_dir.join("subscribe.db")).unwrap();
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
}

pub type SummaryRow = (
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

pub fn read_summary(
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

pub fn adopted_episodes(db: &Connection, media: MediaId) -> Vec<(i64, i64, String)> {
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
