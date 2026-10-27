use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, Media, MediaKind,
    QualitySource, UnitKey,
};
use store::files::FileDetails;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::{LedgerPresenceMode as PresenceMode, LedgerPresenceObservation};
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
        .find(|r| r.library_id == lib.id.clone())
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

#[test]
fn twelve_episodes_two_versions_reduces_to_one_item_twenty_four_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let mut publications = Vec::new();
    let mut files = Vec::new();
    let units_list: Vec<Vec<UnitKey>> = (1..=12).map(|ep| vec![UnitKey::new(1, ep)]).collect();

    // 12 episodes x 2 versions = 24 files
    let mut file_idx = 10;
    for ep in 1..=12 {
        for ver in ["v1", "v2"] {
            file_idx += 1;
            let fid = format!("00000000-0000-0000-0000-{:012x}", file_idx);
            let path = format!("Frieren/S01/Frieren.S01E{:02}.{ver}.mkv", ep);
            let f = make_file(
                &fid,
                &lib.id,
                &root_id,
                media_id,
                &path,
                MediaKind::Tv,
                1_000_000,
            );
            files.push(f);
        }
    }

    for (i, f) in files.iter().enumerate() {
        let ep_idx = i / 2;
        publications.push(StorePublication {
            file: f,
            units: &units_list[ep_idx],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        });
    }

    let report = store
        .publish_files(&scope, &publications, 1000)
        .expect("publish_files");
    assert_eq!(report.inserted, 24);

    // Verify 24 files, 1 item, aggregated size = 24 * 1_000_000 = 24_000_000
    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .expect("query item")
        .expect("item must exist");
    assert_eq!(item.file_count, 24);
    assert_eq!(item.size_bytes, Some(24_000_000));
    assert_eq!(item.distinct_episode_count, 12);
    assert_eq!(item.distinct_season_count, 1);
}

#[test]
fn single_file_multiple_episodes_not_double_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    // 1 file covering E01 and E02
    let fid = "00000000-0000-0000-0000-000000000011";
    let file = make_file(
        fid,
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01-E02.mkv",
        MediaKind::Tv,
        2_000_000,
    );
    let units = vec![UnitKey::new(1, 1), UnitKey::new(1, 2)];

    let pub_item = StorePublication {
        file: &file,
        units: &units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };

    store
        .publish_files(&scope, &[pub_item], 1000)
        .expect("publish multi-episode file");

    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .unwrap();

    assert_eq!(item.file_count, 1, "Must be exactly 1 file");
    assert_eq!(
        item.size_bytes,
        Some(2_000_000),
        "Bytes must not be double counted"
    );
    assert_eq!(item.distinct_episode_count, 2, "Distinct episodes = 2");
}

#[test]
fn pending_file_does_not_produce_playable_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let mut file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
    );
    file.publication = FilePublication::Pending;

    let pub_item = StorePublication {
        file: &file,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };

    store
        .publish_files(&scope, &[pub_item], 1000)
        .expect("publish pending file");

    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap();
    assert!(
        item.is_none(),
        "pending-only library must produce no library_items row"
    );

    // Re-publishing the same file as Published must create the item
    file.publication = FilePublication::Published;
    let pub_published = StorePublication {
        file: &file,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    store
        .publish_files(&scope, &[pub_published], 2000)
        .expect("republish published file");

    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .expect("library item must exist after publishing");
    assert_eq!(item.file_count, 1);
    assert_eq!(item.available_file_count, 1);
    assert_eq!(item.size_bytes, Some(1_000_000));
}
#[test]
fn probe_obligation_created_when_probe_required() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
    );

    let pub_item = StorePublication {
        file: &file,
        units: &[UnitKey::new(1, 1)],
        expected_version: None,
        nfo_observation: None,
        probe_required: true,
    };

    store.publish_files(&scope, &[pub_item], 1000).unwrap();

    // Verify obligation in file_probe_obligations table
    let count = store.count_probe_obligations().unwrap();
    assert_eq!(count, 1);
}

#[test]
fn stale_version_fails_and_rolls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
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

    // Now update with wrong expected version
    let bad_ver = FileVersion {
        revision: 99,
        source_version: "v99".into(),
    };
    let err = store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::new(1, 1)],
                expected_version: Some(&bad_ver),
                nfo_observation: None,
                probe_required: false,
            }],
            2000,
        )
        .expect_err("stale version must fail");

    assert!(matches!(
        err,
        StoreError::Protected(_) | StoreError::Invalid(_)
    ));
}

#[test]
fn presence_verify_marks_missing_and_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
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

    // Observe Missing
    let obs_missing = LedgerPresenceObservation {
        file: store.file_coverage_by_id(file.file_id).unwrap().unwrap(),
        path_key: store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .path_key,
        missing_at: store.missing_at_by_path().unwrap().values().next().copied(),
        presence: FilePresence::Missing,
    };

    let rep = store
        .apply_ledger_presence(&[obs_missing], PresenceMode::Verify, 1500)
        .unwrap();
    assert_eq!(rep.missing, 1);

    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(item.available_file_count, 0);
    assert_eq!(item.missing_file_count, 1);

    // Observe Present (restored)
    let obs_present = LedgerPresenceObservation {
        file: store.file_coverage_by_id(file.file_id).unwrap().unwrap(),
        path_key: store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .path_key,
        missing_at: store.missing_at_by_path().unwrap().values().next().copied(),
        presence: FilePresence::Present,
    };

    let rep2 = store
        .apply_ledger_presence(&[obs_present], PresenceMode::Verify, 2000)
        .unwrap();
    assert_eq!(rep2.restored, 1);

    let item2 = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(item2.available_file_count, 1);
    assert_eq!(item2.missing_file_count, 0);
}

#[test]
fn publish_file_details_persists_details_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
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

    let details = FileDetails {
        source_revision: 1,
        source_version: "v1".into(),
        tracks_version: 1,
        video_json: Some(r#"{"codec":"hevc","width":1920,"height":1080}"#.into()),
        audio_json: r#"[{"codec":"aac","channels":2}]"#.into(),
        subtitle_json: r#"[]"#.into(),
        chapters_json: None,
        fingerprint_json: None,
        outro_fingerprint_json: None,
        format_duration_ms: Some(120_000),
    };

    store
        .publish_file_details(file.file_id, &file.version, &details, 2000)
        .unwrap();

    let fetched = store
        .get_file_details(file.file_id)
        .unwrap()
        .expect("details must exist");
    assert_eq!(fetched.source_version, "v1");
    assert_eq!(fetched.format_duration_ms, Some(120_000));
}

#[test]
fn deleting_all_files_cleans_item_but_preserves_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");

    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let file = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
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

    assert!(
        store
            .get_library_item(scope.library_id, media_id.parse().unwrap())
            .unwrap()
            .is_some()
    );

    // Prune the file
    let obs = LedgerPresenceObservation {
        file: store.file_coverage_by_id(file.file_id).unwrap().unwrap(),
        path_key: store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .path_key,
        missing_at: store.missing_at_by_path().unwrap().values().next().copied(),
        presence: FilePresence::Missing,
    };
    store
        .apply_ledger_presence(&[obs.clone()], PresenceMode::Verify, 1500)
        .unwrap();
    let mut obs = obs;
    obs.missing_at = Some(1500);
    store
        .apply_ledger_presence(&[obs], PresenceMode::Prune, 2000)
        .unwrap();

    // Item must be cleaned up because 0 files remain
    assert!(
        store
            .get_library_item(scope.library_id, media_id.parse().unwrap())
            .unwrap()
            .is_none()
    );

    // But Canonical Media must still exist!
    let m = store.get_media(media_id.parse().unwrap()).unwrap();
    assert!(m.is_some(), "Media fact must survive file deletion");
}

/// A Transfer reservation and its publication carry the same `file_id`, so the
/// destination identity a failed round already recorded (root, source path, mode)
/// survives the completion call even when that call does not repeat it.
#[test]
fn publication_keeps_the_destination_identity_recorded_by_the_reservation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000002";
    create_media(&store, media_id, "Frieren", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "Anime TV", MediaKind::Tv, "/anime");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };

    let mut reserved = make_file(
        "00000000-0000-0000-0000-000000000011",
        &lib.id,
        &root_id,
        media_id,
        "Frieren/S01E01.mkv",
        MediaKind::Tv,
        1_000_000,
    );
    reserved.publication = FilePublication::Pending;
    reserved.source_path = Some("/downloads/Frieren.S01E01.mkv".into());
    reserved.transfer_mode = Some("copy".to_string());
    let units = [UnitKey::new(1, 1)];
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &reserved,
                units: &units,
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .expect("reserve the destination");

    let mut published = reserved.clone();
    published.publication = FilePublication::Published;
    published.source_path = None;
    published.transfer_mode = None;
    let report = store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &published,
                units: &units,
                expected_version: None,
                nfo_observation: None,
                probe_required: true,
            }],
            2000,
        )
        .expect("complete the reservation");
    assert_eq!(report.updated, 1, "the completion updates the reserved row");

    let library_path = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let conn = rusqlite::Connection::open(library_path).unwrap();
    let (root, source, mode, publication): (String, Option<String>, Option<String>, String) = conn
        .query_row(
            "SELECT root_id, source_path, transfer_mode, publication_state
             FROM library_files WHERE file_id = ?1",
            [published.file_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the file row exists");
    conn.close().unwrap();

    assert_eq!(root, root_id);
    assert_eq!(source.as_deref(), Some("/downloads/Frieren.S01E01.mkv"));
    assert_eq!(mode.as_deref(), Some("copy"));
    assert_eq!(publication, "published");
    assert_eq!(store.count_probe_obligations().unwrap(), 1);
    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .expect("published file produces the item summary");
    assert_eq!(item.file_count, 1);
    assert_eq!(item.available_file_count, 1);
}

fn unit_rows(tmp: &std::path::Path) -> Vec<(i32, i32, i64, i64, Option<String>)> {
    let conn = rusqlite::Connection::open(tmp.join("library.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT season, episode, file_count, available_file_count, preferred_file_id
             FROM library_units ORDER BY season, episode",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// An existing card gains a new episode through two versions in one batch: the
/// new unit is counted once per file, and the incremental result matches a
/// full reduction of the same facts.
#[test]
fn existing_card_batch_two_versions_of_new_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id = "00000000-0000-0000-0000-0000000000e2";
    create_media(&store, media_id, "Batch Show", MediaKind::Tv);
    let (lib, root_id) = create_library(&store, "TV", MediaKind::Tv, "/tv");
    let scope = WriteScope {
        library_id: lib.id.parse().unwrap(),
        config_revision: 0,
    };
    let e01 = make_file(
        "00000000-0000-0000-0000-0000000000f1",
        &lib.id,
        &root_id,
        media_id,
        "Batch/S01E01.mkv",
        MediaKind::Tv,
        1_000,
    );
    let e01_units = [UnitKey::new(1, 1)];
    let publication = |file, units| StorePublication {
        file,
        units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    };
    store
        .publish_files(&scope, &[publication(&e01, &e01_units)], 1_000)
        .unwrap();

    let a = make_file(
        "00000000-0000-0000-0000-0000000000f2",
        &lib.id,
        &root_id,
        media_id,
        "Batch/S01E02.a.mkv",
        MediaKind::Tv,
        2_000,
    );
    let b = make_file(
        "00000000-0000-0000-0000-0000000000f3",
        &lib.id,
        &root_id,
        media_id,
        "Batch/S01E02.b.mkv",
        MediaKind::Tv,
        3_000,
    );
    let e02_units = [UnitKey::new(1, 2)];
    store
        .publish_files(
            &scope,
            &[publication(&a, &e02_units), publication(&b, &e02_units)],
            2_000,
        )
        .unwrap();

    let incremental = unit_rows(tmp.path());
    let e02 = incremental
        .iter()
        .find(|row| (row.0, row.1) == (1, 2))
        .unwrap();
    assert_eq!((e02.2, e02.3), (2, 2));
    let item = store
        .get_library_item(scope.library_id, media_id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!((item.file_count, item.distinct_episode_count), (3, 2));

    store
        .refresh_media_summaries(media_id.parse().unwrap(), 3_000)
        .unwrap();
    assert_eq!(
        unit_rows(tmp.path()),
        incremental,
        "incremental == full reduce"
    );
}
