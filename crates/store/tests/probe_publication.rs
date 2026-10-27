//! Probe result publication: hot quality columns, cold `file_details`, derived card summary.
//!
//! A probe result is only publishable while the file row still carries the version the worker
//! probed; anything else is a stale worker and must change nothing.

use std::path::Path;

use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaKind,
    QualitySource, UnitKey,
};
use library::{ChapterMarker, MarkerType, Tracks};
use store::Store;
use store::files::FileDetails;
use store::library_write::probe::{CURRENT_TRACKS_VERSION, ProbePublication};
use store::library_write::{FilePublication as StorePublication, WriteScope};

const MEDIA_ID: &str = "00000000-0000-0000-0000-0000000000aa";
const FILE_ID: &str = "00000000-0000-0000-0000-0000000000bb";

struct Fixture {
    store: Store,
    library_id: String,
    file: LibraryFile,
}

fn create_media(store: &Store, id: &str, title: &str, kind: MediaKind) {
    store
        .insert_media(&Media {
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
        })
        .unwrap();
}

fn make_file(
    file_id: &str,
    library_id: &str,
    root_id: &str,
    media_id: &str,
    path: &str,
    version: FileVersion,
) -> LibraryFile {
    LibraryFile {
        file_id: file_id.parse().unwrap(),
        library_id: library_id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id: media_id.parse().unwrap(),
        kind: MediaKind::Movie,
        path: path.into(),
        version,
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(4_000_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash1".to_string()),
        source_path: None,
        transfer_mode: None,
        resolution: Some("720p".to_string()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: Some("720p WEB-DL".to_string()),
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

/// One published movie file whose quality is still the release-name guess.
fn seed(dir: &Path) -> Fixture {
    let store = Store::open(dir).unwrap();
    create_media(&store, MEDIA_ID, "Probe Me", MediaKind::Movie);
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|entry| entry.library_id.as_str() == library.id.as_str())
        .unwrap()
        .id;
    let file = make_file(
        FILE_ID,
        &library.id,
        &root_id,
        MEDIA_ID,
        "/movies/probe-me.mkv",
        FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
    );
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    Fixture {
        store,
        library_id: library.id,
        file,
    }
}

/// Read one committed column from `library.db` — the observable storage fact.
fn read<T: rusqlite::types::FromSql>(
    dir: &Path,
    sql: &str,
    params: impl rusqlite::Params,
) -> Option<T> {
    let conn = rusqlite::Connection::open(dir.join("library.db")).unwrap();
    conn.query_row(sql, params, |row| row.get(0)).ok()
}

fn query_row<T>(
    dir: &Path,
    sql: &str,
    params: impl rusqlite::Params,
    f: impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Option<T> {
    let conn = rusqlite::Connection::open(dir.join("library.db")).unwrap();
    conn.query_row(sql, params, f).ok()
}

fn video_details(version: &FileVersion, video_json: &str, duration_ms: Option<i64>) -> FileDetails {
    FileDetails {
        source_revision: version.revision,
        source_version: version.source_version.clone(),
        tracks_version: CURRENT_TRACKS_VERSION,
        video_json: Some(video_json.to_string()),
        audio_json: r#"[{"codec":"aac","channels":2}]"#.into(),
        subtitle_json: r#"[]"#.into(),
        chapters_json: None,
        fingerprint_json: None,
        outro_fingerprint_json: None,
        format_duration_ms: duration_ms,
    }
}

#[test]
fn probe_publication_refreshes_quality_and_card_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fixture = seed(dir);
    let store = &fixture.store;

    let item_quality = query_row(
        dir,
        "SELECT resolution_rank, hdr FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        rusqlite::params![&fixture.library_id, MEDIA_ID],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    assert_eq!(
        item_quality,
        Some((2, 0)),
        "before the probe the card only knows the 720p release guess"
    );

    let expected = fixture.file.version.clone();
    let details = video_details(
        &expected,
        r#"{"codec":"hevc","width":3840,"height":2160,"color_transfer":"smpte2084"}"#,
        Some(7_200_000),
    );
    let publication = ProbePublication {
        expected: &expected,
        tracks_version: CURRENT_TRACKS_VERSION,
        resolution: Some("2160p"),
        codec: Some("hevc"),
        hdr: Some("HDR10"),
        duration_ms: Some(7_200_000),
        details: Some(&details),
    };
    assert!(
        store
            .publish_probe_result(fixture.file.file_id, &publication, 2_000)
            .unwrap()
    );

    let hot = query_row(
        dir,
        "SELECT resolution, codec, hdr, duration_ms, tracks_version, quality_source
         FROM library_files WHERE file_id = ?1",
        rusqlite::params![FILE_ID],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    );
    assert_eq!(
        hot,
        Some((
            "2160p".to_string(),
            "hevc".to_string(),
            "HDR10".to_string(),
            7_200_000,
            CURRENT_TRACKS_VERSION,
            "probe".to_string(),
        )),
        "probed stream facts replace the release guess"
    );

    let item_quality = query_row(
        dir,
        "SELECT resolution_rank, hdr FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        rusqlite::params![&fixture.library_id, MEDIA_ID],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    assert_eq!(
        item_quality,
        Some((4, 1)),
        "the same transaction moved the card to 2160p + HDR"
    );
    let resolution_terms: Option<i64> = read(
        dir,
        "SELECT COUNT(*) FROM library_item_terms
         WHERE library_id = ?1 AND media_id = ?2 AND term_kind = 'resolution' AND term_value = '2160p'",
        rusqlite::params![&fixture.library_id, MEDIA_ID],
    );
    assert_eq!(resolution_terms, Some(1));

    let tracks = store
        .get_file_meta(FILE_ID)
        .unwrap()
        .expect("probed tracks");
    assert_eq!(tracks.video.unwrap().height, Some(2160));
}

#[test]
fn stale_probe_result_is_rejected_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fixture = seed(dir);
    let store = &fixture.store;

    let stale = FileVersion {
        revision: 99,
        source_version: "v99".to_string(),
    };
    let details = video_details(
        &stale,
        r#"{"codec":"hevc","width":3840,"height":2160,"color_transfer":"smpte2084"}"#,
        Some(7_200_000),
    );
    let publication = ProbePublication {
        expected: &stale,
        tracks_version: CURRENT_TRACKS_VERSION,
        resolution: Some("2160p"),
        codec: Some("hevc"),
        hdr: Some("HDR10"),
        duration_ms: Some(7_200_000),
        details: Some(&details),
    };
    assert!(
        !store
            .publish_probe_result(fixture.file.file_id, &publication, 2_000)
            .unwrap(),
        "a result for another revision must be refused, not written"
    );

    let hot = query_row(
        dir,
        "SELECT resolution, codec, hdr, duration_ms, quality_source FROM library_files WHERE file_id = ?1",
        rusqlite::params![FILE_ID],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    );
    assert_eq!(
        hot,
        Some(("720p".to_string(), None, None, None, "release".to_string(),))
    );
    let details_rows: Option<i64> = read(
        dir,
        "SELECT COUNT(*) FROM file_details WHERE file_id = ?1",
        rusqlite::params![FILE_ID],
    );
    assert_eq!(details_rows, Some(0));
    let item_quality = query_row(
        dir,
        "SELECT resolution_rank, hdr FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        rusqlite::params![&fixture.library_id, MEDIA_ID],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    assert_eq!(item_quality, Some((2, 0)));
}

#[test]
fn cached_chapters_round_trip_and_ignore_a_replaced_file_version() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fixture = seed(dir);
    let store = &fixture.store;

    let chapters = vec![ChapterMarker {
        start_ms: 0,
        end_ms: 60_000,
        title: Some("片头".into()),
        marker_type: Some(MarkerType::IntroStart),
        synthetic: false,
    }];
    store.put_cached_chapters(FILE_ID, &chapters).unwrap();
    assert_eq!(store.get_cached_chapters(FILE_ID).unwrap(), Some(chapters));

    // The file is replaced: a chapter cache written for the old bytes must not be served.
    let scope = WriteScope {
        library_id: fixture.library_id.parse().unwrap(),
        config_revision: 0,
    };
    let replaced = make_file(
        FILE_ID,
        &fixture.library_id,
        &fixture.file.root_id,
        MEDIA_ID,
        "/movies/probe-me.mkv",
        FileVersion {
            revision: 2,
            source_version: "v2".to_string(),
        },
    );
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &replaced,
                units: &[UnitKey::WHOLE],
                expected_version: Some(&fixture.file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            3_000,
        )
        .unwrap();

    let fresh = vec![ChapterMarker {
        start_ms: 10_000,
        end_ms: 90_000,
        title: Some("新片头".into()),
        marker_type: Some(MarkerType::IntroStart),
        synthetic: false,
    }];
    store.put_cached_chapters(FILE_ID, &fresh).unwrap();
    assert_eq!(
        store.get_cached_chapters(FILE_ID).unwrap(),
        None,
        "a chapter cache that describes the replaced file is not served"
    );
    let stored_version: Option<String> = read(
        dir,
        "SELECT source_version FROM file_details WHERE file_id = ?1",
        rusqlite::params![FILE_ID],
    );
    assert_eq!(
        stored_version,
        Some("v1".to_string()),
        "the stale write must not relabel the cached row"
    );

    // Once the file's stream facts are republished for the new version, chapters apply again.
    store
        .put_file_meta_versioned(
            FILE_ID,
            &Tracks {
                video: None,
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
            Some("v2"),
            None,
        )
        .unwrap();
    store.put_cached_chapters(FILE_ID, &fresh).unwrap();
    assert_eq!(store.get_cached_chapters(FILE_ID).unwrap(), Some(fresh));

    store.clear_cached_chapters(FILE_ID).unwrap();
    assert_eq!(store.get_cached_chapters(FILE_ID).unwrap(), None);
}
