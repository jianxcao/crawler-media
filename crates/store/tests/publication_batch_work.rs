//! Q2: a publication batch finalizes each user summary once per Media, not once
//! per file. Measured as the extra time user facts add to a 100-file batch.

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media, MediaId,
    MediaKind, QualitySource, UnitKey, UserId,
};
use std::time::Instant;
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::playback::PlaybackMutation;

fn file(lib: &store::Library, root: &str, media: MediaId, ep: u32) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id: lib.id.parse().unwrap(),
        root_id: root.into(),
        media_id: media,
        kind: MediaKind::Tv,
        path: format!("Show/S01E{ep:03}.mkv").into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(1_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn pubs(files: &[(LibraryFile, [UnitKey; 1])]) -> Vec<StorePublication<'_>> {
    files
        .iter()
        .map(|(f, u)| StorePublication {
            file: f,
            units: u,
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect()
}

/// Returns (summary writes during the batch, owned_episode_count of the first user).
fn batch(users: usize, protocol: bool) -> (u64, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let lib = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == lib.id.clone())
        .unwrap()
        .id;
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let first = [(file(&lib, &root, media.id, 0), [UnitKey::new(1, 0)])];
    store.publish_files(&scope, &pubs(&first), 1_000).unwrap();
    let ids: Vec<UserId> = (0..users).map(|_| UserId::new()).collect();
    for user in &ids {
        store
            .mutate_playback(
                media.id,
                &PlaybackMutation::marks(*user, 1, 0, Some(true), None),
                2_000,
            )
            .unwrap();
    }
    let db = measure_protocol_writes(tmp.path().join("library.db"), media.id, protocol);
    let files: Vec<_> = (1..=100)
        .map(|ep| {
            (
                file(&lib, &root, media.id, ep),
                [UnitKey::new(1, ep as i32)],
            )
        })
        .collect();
    let start = store::work_counters::user_summary_writes();
    let publication_start = Instant::now();
    store.publish_files(&scope, &pubs(&files), 3_000).unwrap();
    let publication_ms = publication_start.elapsed().as_secs_f64() * 1_000.;
    let ms = store::work_counters::user_summary_writes() - start;
    let db_path = tmp.path().join("library.db");
    println!(
        "publication users={users} protocol_triggers={protocol} files=100 publication_ms={publication_ms:.3} db_bytes={} wal_bytes={:?} summary_writes={ms}",
        std::fs::metadata(&db_path).unwrap().len(),
        std::fs::metadata(format!("{}-wal", db_path.display()))
            .map(|m| m.len())
            .ok()
    );
    check_protocol_writes(&db, media.id, protocol);
    if protocol {
        check_indexed_queries(&store, lib.id.parse().unwrap());
        store
            .delete_files(scope.library_id, &[first[0].0.file_id])
            .unwrap();
        demote_batch(&store, &scope, files.clone(), media.id);
    }
    drop(db);
    drop(store);
    let owned = ids
        .first()
        .map(|user| {
            rusqlite::Connection::open(tmp.path().join("library.db"))
                .unwrap()
                .query_row(
                    "SELECT owned_episode_count FROM user_library_items WHERE user_id=?1",
                    [user.to_string()],
                    |r| r.get(0),
                )
                .unwrap()
        })
        .unwrap_or(0);
    (ms, owned)
}

#[test]
fn batch_user_summary_cost_does_not_scale_with_files_times_users() {
    let (quiet, _) = batch(0, false);
    let (busy, owned) = batch(40, false);
    println!("summary writes for a 100-file batch: 0 users {quiet}, 40 users {busy}");
    assert_eq!(owned, 101, "summary reflects every file of the batch");
    assert_eq!(quiet, 0);
    assert_eq!(busy, 40, "one finalize per user per Media per batch");
}

#[test]
fn batch_protocol_index_cost_tracks_cards_instead_of_files_times_people() {
    batch(0, true);
}

fn measure_protocol_writes(
    path: std::path::PathBuf,
    media: MediaId,
    protocol: bool,
) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(path).unwrap();
    if protocol {
        let cast: Vec<_> = (0..100)
            .map(|i| serde_json::json!({"name": format!("Actor {i}")}))
            .collect();
        let fields = serde_json::json!({"fields": {"cast": {"state": "value", "value": cast}}});
        db.execute(
            "INSERT INTO media_metadata(media_id,locale,title,details_json,revision)
                    VALUES(?1,'zh-CN','Show',?2,1)",
            rusqlite::params![media.to_string(), fields.to_string()],
        )
        .unwrap();
        db.execute_batch("CREATE TABLE measured_protocol_writes(kind TEXT);
            CREATE TRIGGER measured_person_insert AFTER INSERT ON library_protocol_people BEGIN INSERT INTO measured_protocol_writes VALUES('insert'); END;
            CREATE TRIGGER measured_person_delete AFTER DELETE ON library_protocol_people BEGIN INSERT INTO measured_protocol_writes VALUES('delete'); END;").unwrap();
    }
    db
}

fn check_protocol_writes(db: &rusqlite::Connection, media: MediaId, protocol: bool) {
    if protocol {
        let inserts: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM measured_protocol_writes WHERE kind='insert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let deletes: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM measured_protocol_writes WHERE kind='delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let terms: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM library_protocol_people WHERE media_id=?1",
                [media.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(terms, 100);
        assert_eq!(
            inserts, 100,
            "one protocol index publication per Media per batch"
        );
        assert_eq!(deletes, 0, "no intermediate unpublished protocol indexes");
    }
}

fn check_indexed_queries(store: &Store, library: domain::LibraryId) {
    let mut query = domain::ProtocolItemQuery {
        user_id: UserId::new(),
        parent: domain::ProtocolParent::Library(library),
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: vec![domain::ProtocolItemType::Series],
        recursive: true,
        is_folder: None,
        sort: vec![],
        offset: 0,
        limit: 10,
    };
    let plain = store.protocol_item_page(&query).unwrap();
    query.person_ids =
        vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:actor 0:Actor").to_string()];
    assert_eq!(store.protocol_item_page(&query).unwrap(), plain);
    query.person_ids.clear();
    query.search_term = Some("show".into());
    assert_eq!(store.protocol_item_page(&query).unwrap(), plain);
}

fn demote_batch(
    store: &Store,
    scope: &WriteScope,
    mut files: Vec<(LibraryFile, [UnitKey; 1])>,
    media: MediaId,
) {
    for (file, _) in &mut files {
        file.publication = FilePublication::Pending;
    }
    store.publish_files(scope, &pubs(&files), 4_000).unwrap();
    assert!(
        store
            .get_library_item(scope.library_id, media)
            .unwrap()
            .is_none(),
        "a pending-only batch removes the last card and its protocol index"
    );
}
