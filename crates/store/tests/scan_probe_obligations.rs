use domain::{Confidence, LedgerId, MediaId, QualitySource};
use store::Store;

/// Fixture scan row: a whole-unit file with a release observation and no NFO facts.
fn sample_row(media_id: MediaId, path: &str) -> store::ScannedLedger {
    store::ScannedLedger {
        file_id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        units: vec![domain::UnitKey::WHOLE],
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        source_version: "fixture-v1".into(),
        source_digest: None,
        size_bytes: None,
        created_ns: None,
        modified_ns: None,
        artwork: Default::default(),
        observations: Vec::new(),
    }
}

fn setup(data_dir: &std::path::Path) -> (Store, store::Library, MediaId) {
    let store = Store::open(data_dir).unwrap();
    let lib = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &["/movies"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Test Movie".to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    (store, lib, media.id)
}

#[test]
fn insert_and_obligation_are_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let (store, _lib, media_id) = setup(&data_dir);

    let row = sample_row(media_id, "/movies/test.mkv");

    // Add trigger on library.db that fails on obligation insert
    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "CREATE TRIGGER fail_ob_insert BEFORE INSERT ON file_probe_obligations
             BEGIN
                 SELECT RAISE(ABORT, 'forced obligation fail');
             END;",
            [],
        )
        .unwrap();

    let res = store.insert_scanned_files(std::slice::from_ref(&row), 1000);
    assert!(res.is_err(), "atomic insert must fail due to trigger");

    // Ledger row must NOT exist because transaction rolled back
    assert!(store.file_coverage_by_id(row.file_id).unwrap().is_none());
}

#[test]
fn ledger_cleanup_removes_obligation_not_user_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, lib, media_id) = setup(&tmp.path().join("data"));

    let row = sample_row(media_id, "/movies/clean.mkv");
    store
        .insert_scanned_files(std::slice::from_ref(&row), 1000)
        .unwrap();

    // Verify obligation exists
    let obs = store.scan_probe_obligations(10).unwrap();
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].ledger_id, row.file_id);

    // Delete files via Store::delete_files
    let removed = store
        .delete_files(lib.id.parse().unwrap(), &[row.file_id])
        .unwrap();
    assert_eq!(removed, 1);

    // Ledger and obligation must both be gone
    assert!(store.file_coverage_by_id(row.file_id).unwrap().is_none());
    let obs_after = store.scan_probe_obligations(10).unwrap();
    assert!(
        obs_after.is_empty(),
        "obligation must be deleted on ledger cleanup"
    );
}

#[test]
fn ack_removes_obligation() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _lib, media_id) = setup(&tmp.path().join("data"));

    let row = sample_row(media_id, "/movies/ack.mkv");
    store
        .insert_scanned_files(std::slice::from_ref(&row), 1000)
        .unwrap();

    assert_eq!(store.scan_probe_obligations(10).unwrap().len(), 1);
    store.ack_scan_probe_obligation(row.file_id).unwrap();
    assert_eq!(store.scan_probe_obligations(10).unwrap().len(), 0);
}
