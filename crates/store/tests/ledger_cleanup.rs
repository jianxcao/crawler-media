//! Deleting a Library file removes its file-derived rows and nothing else:
//! Media identity, user playback facts, collections and other files survive.

use std::path::Path;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, Media,
    MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use rusqlite::Connection;
use store::files::FileDetails;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

fn create_media(store: &Store, title: &str, kind: MediaKind) -> Media {
    let media = Media {
        id: MediaId::new(),
        kind,
        title: title.to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media
}

fn create_library(store: &Store, root: &Path) -> (store::Library, String) {
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_deref() == Some(library.id.as_str()))
        .unwrap()
        .id;
    (library, root_id)
}

fn make_file(
    library: &store::Library,
    root_id: &str,
    media: MediaId,
    name: &str,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id: media,
        kind: MediaKind::Movie,
        path: name.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1_000),
        stat_known: true,
        created_ns: Some(10),
        modified_ns: Some(20),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".to_string()),
        codec: Some("hevc".to_string()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn publish(store: &Store, library: &store::Library, file: &LibraryFile) {
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
}

fn publish_details(store: &Store, file: &LibraryFile) {
    let details = FileDetails {
        source_revision: file.version.revision,
        source_version: file.version.source_version.clone(),
        tracks_version: 1,
        video_json: Some(r#"{"codec":"hevc"}"#.into()),
        audio_json: "[]".into(),
        subtitle_json: "[]".into(),
        chapters_json: None,
        fingerprint_json: None,
        outro_fingerprint_json: None,
        format_duration_ms: Some(60_000),
    };
    store
        .publish_file_details(file.file_id, &file.version, &details, 1_000)
        .unwrap();
}

fn library_db(path: &Path) -> Connection {
    Connection::open(path.join("data").join("library.db")).unwrap()
}

fn sidecar_count(conn: &Connection, file_id: LedgerId) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM scrape_sidecars WHERE file_id = ?1",
        [file_id.to_string()],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn deleting_a_file_removes_only_its_derived_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let movies = create_media(&store, "Matrix", MediaKind::Movie);
    let other = create_media(&store, "Alien", MediaKind::Movie);
    let deleted = make_file(&library, &root_id, movies.id, "Matrix.mkv");
    let kept = make_file(&library, &root_id, other.id, "Alien.mkv");
    for file in [&deleted, &kept] {
        publish(&store, &library, file);
        publish_details(&store, file);
    }

    let user = UserId::new();
    store
        .set_playback_state(user, movies.id, 500, Some(true), None, None, None)
        .unwrap();

    let conn = library_db(tmp.path());
    for file in [&deleted, &kept] {
        conn.execute(
            "INSERT INTO scrape_sidecars (file_id, kind, path) VALUES (?1, 'nfo', ?2)",
            rusqlite::params![file.file_id.to_string(), format!("{}.nfo", file.path.display())],
        )
        .unwrap();
    }

    let removed = store
        .delete_files(library.id.parse().unwrap(), &[deleted.file_id])
        .unwrap();
    assert_eq!(removed, 1);

    // The file, its details, its sidecars and its card are gone.
    assert!(store.file_coverage_by_id(deleted.file_id).unwrap().is_none());
    assert!(store.get_file_details(deleted.file_id).unwrap().is_none());
    assert_eq!(sidecar_count(&conn, deleted.file_id), 0);
    assert!(
        store
            .get_library_item(library.id.parse().unwrap(), movies.id)
            .unwrap()
            .is_none(),
        "the last file of an item removes its card"
    );

    // The other file, the Media and the user's watch state stay.
    assert!(store.file_coverage_by_id(kept.file_id).unwrap().is_some());
    assert!(store.get_file_details(kept.file_id).unwrap().is_some());
    assert_eq!(sidecar_count(&conn, kept.file_id), 1);
    assert!(
        store
            .get_library_item(library.id.parse().unwrap(), other.id)
            .unwrap()
            .is_some()
    );
    assert!(store.get_media(movies.id).unwrap().is_some());
    assert_eq!(
        store.playback_progress(user, movies.id).unwrap(),
        Some(500)
    );
}

#[test]
fn repeated_delete_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let media = create_media(&store, "Matrix", MediaKind::Movie);
    let file = make_file(&library, &root_id, media.id, "Matrix.mkv");
    publish(&store, &library, &file);

    let library_id = library.id.parse().unwrap();
    assert_eq!(store.delete_files(library_id, &[file.file_id]).unwrap(), 1);
    assert_eq!(store.delete_files(library_id, &[file.file_id]).unwrap(), 0);
    // Unknown ids and foreign ids are ignored, never counted.
    assert_eq!(
        store.delete_files(library_id, &[LedgerId::new()]).unwrap(),
        0
    );
}

#[test]
fn derived_delete_failure_rolls_back_every_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let media = create_media(&store, "Matrix", MediaKind::Movie);
    let file = make_file(&library, &root_id, media.id, "Matrix.mkv");
    publish(&store, &library, &file);
    publish_details(&store, &file);

    let conn = library_db(tmp.path());
    conn.execute(
        "INSERT INTO scrape_sidecars (file_id, kind, path) VALUES (?1, 'nfo', '/movies/Matrix.nfo')",
        [file.file_id.to_string()],
    )
    .unwrap();
    conn.execute(
        "CREATE TRIGGER fail_sidecar_del BEFORE DELETE ON scrape_sidecars
         BEGIN
             SELECT RAISE(ABORT, 'forced trigger abort');
         END;",
        [],
    )
    .unwrap();

    let result = store.delete_files(library.id.parse().unwrap(), &[file.file_id]);
    assert!(result.is_err(), "the trigger must fail the whole deletion");
    assert!(store.file_coverage_by_id(file.file_id).unwrap().is_some());
    assert!(store.get_file_details(file.file_id).unwrap().is_some());
    assert_eq!(sidecar_count(&conn, file.file_id), 1);
}

#[test]
fn deleting_every_file_of_a_media_keeps_media_and_playback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let media = create_media(&store, "Matrix", MediaKind::Movie);
    let part_one = make_file(&library, &root_id, media.id, "Matrix.part1.mkv");
    let part_two = make_file(&library, &root_id, media.id, "Matrix.part2.mkv");
    for file in [&part_one, &part_two] {
        publish(&store, &library, file);
    }
    let user = UserId::new();
    store
        .set_playback_state(user, media.id, 900, Some(true), None, None, None)
        .unwrap();

    assert_eq!(store.delete_ledger_for_media(media.id).unwrap(), 2);
    assert!(store.file_coverage_by_id(part_one.file_id).unwrap().is_none());
    assert!(store.file_coverage_by_id(part_two.file_id).unwrap().is_none());
    assert!(store.get_media(media.id).unwrap().is_some());
    assert_eq!(
        store.playback_progress(user, media.id).unwrap(),
        Some(900)
    );
}

#[test]
fn deleting_a_file_retires_queued_probe_work() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let (library, root_id) = create_library(&store, tmp.path());
    let media = create_media(&store, "Matrix", MediaKind::Movie);
    let file = make_file(&library, &root_id, media.id, "Matrix.mkv");
    store
        .publish_files(
            &WriteScope {
                library_id: library.id.parse().unwrap(),
                config_revision: 0,
            },
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: true,
            }],
            1_000,
        )
        .unwrap();
    assert_eq!(store.count_probe_obligations().unwrap(), 1);

    let unit_id = file.file_id.to_string();
    store
        .create_probe_job(
            "job-delete",
            "media_probe",
            &media.id.to_string(),
            None,
            "delete:one",
            &[store::ProbeJobUnitSpec {
                ledger_id: &unit_id,
                kind: "movie",
                force_fingerprint: false,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: false,
            }],
        )
        .unwrap();
    store.start_probe_unit("job-delete", &unit_id).unwrap();

    store
        .delete_files(library.id.parse().unwrap(), &[file.file_id])
        .unwrap();

    assert!(store.probe_job_units("job-delete").unwrap().is_empty());
    let job = store.get_probe_job("job-delete").unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    assert_eq!(store.count_probe_obligations().unwrap(), 0);
}
