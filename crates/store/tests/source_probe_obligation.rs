use domain::{LibraryFile, Media, MediaId, MediaKind};
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{FilePresence, LedgerPresenceMode, LedgerPresenceObservation, Store};

fn fixture() -> (tempfile::TempDir, Store, LibraryFile) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir(&root).unwrap();
    let data = tmp.path().join("data");
    let store = store::Store::open(&data).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Movie".into(),
        original_title: None,
        year: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("movie.strm");
    std::fs::write(&path, "https://example.invalid/first.mkv").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        },
    )
    .unwrap();
    (tmp, store, file)
}

#[test]
fn replacing_source_keeps_a_durable_obligation_safe_from_previous_source_ack() {
    let (tmp, store, file) = fixture();
    let data = tmp.path().join("data");
    let path = &file.path;
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    store
        .arm_scan_probe_obligation(&file.file_id.to_string(), 100, "publish")
        .unwrap();
    let mut expected = file.version;
    for url in [
        "https://example.invalid/second.mkv",
        "https://example.invalid/third.mkv",
    ] {
        let old = store.scan_probe_obligations(10).unwrap().remove(0);
        let job_id = format!("source-{}", expected.revision);
        assert!(
            store
                .create_probe_job(
                    &job_id,
                    "media_probe",
                    &file.media_id.to_string(),
                    None,
                    "ledger",
                    &[store::ProbeJobUnitSpec {
                        ledger_id: &file.file_id.to_string(),
                        kind: "movie",
                        force_fingerprint: false,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap()
        );
        std::fs::write(path, url).unwrap();
        expected = store
            .replace_file_source(
                &scope,
                file.file_id,
                &expected,
                &library::observe_source(path).unwrap(),
                100,
            )
            .unwrap();
        assert!(
            !store.ack_scan_probe_obligation_if_current(&old).unwrap(),
            "an ACK assessed for previous content cannot consume the new source's probe obligation"
        );
        assert!(
            store
                .active_probe_unit_for_ledger(&file.file_id.to_string())
                .unwrap()
                .is_none(),
            "work for the previous source cannot satisfy the new source's obligation"
        );
        assert_eq!(
            store.get_probe_job(&job_id).unwrap().unwrap().status,
            "cancelled"
        );
        assert_eq!(
            store.probe_job_units(&job_id).unwrap()[0].status,
            "cancelled"
        );
    }
    drop(store);
    let store = store::Store::open(&data).unwrap();
    let remaining = store.scan_probe_obligations(10).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].ledger_id, file.file_id);
    assert_eq!(
        store.get_file_version(file.file_id).unwrap(),
        Some(expected)
    );
}

#[test]
fn delayed_missing_source_ack_preserves_the_restored_obligation() {
    let (_tmp, store, file) = fixture();
    let file_id = file.file_id.to_string();
    let apply_presence = |presence, missing_at, now_ms| {
        let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        store
            .apply_ledger_presence(
                &[LedgerPresenceObservation {
                    path_key: row.path_key.clone(),
                    file: row,
                    presence,
                    missing_at,
                    observed_source: None,
                }],
                LedgerPresenceMode::Verify,
                now_ms,
            )
            .unwrap()
    };
    store
        .arm_scan_probe_obligation(&file_id, 100, "publish")
        .unwrap();
    let old = store.scan_probe_obligations(10).unwrap().remove(0);
    assert_eq!(apply_presence(FilePresence::Missing, None, 200).missing, 1);
    assert!(store.ledger_is_missing(&file_id).unwrap());
    assert_eq!(
        apply_presence(FilePresence::Present, Some(200), 300).restored,
        1
    );

    assert!(
        !store
            .discard_scan_probe_obligation_if_source_unavailable(&old)
            .unwrap()
    );

    let remaining = store.scan_probe_obligations(10).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].ledger_id, file.file_id);
    assert_eq!(remaining[0].reason, "source_restored");
}

#[test]
fn scan_probe_obligations_after_pages_with_tie_breaker_and_constant_work() {
    let (tmp, store, first_file) = fixture();
    let root = tmp.path().join("movies");
    let media_id = first_file.media_id;
    let mut files = vec![first_file];
    for idx in 0..8 {
        let p = root.join(format!("item_{idx}.strm"));
        std::fs::write(&p, format!("https://example.invalid/{idx}.mkv")).unwrap();
        files.push(
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: p.to_string_lossy().into_owned(),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
    }
    for (idx, f) in files.iter().enumerate() {
        // Group 0..=2 at t=100, 3..=5 at t=200, 6..=8 at t=300 to test identical timestamps.
        let ts = ((idx / 3) as i64 + 1) * 100;
        store
            .arm_scan_probe_obligation(&f.file_id.to_string(), ts, "page")
            .unwrap();
    }

    let mut collected = Vec::new();
    let mut cursor = None;
    loop {
        let page = store.scan_probe_obligations_after(cursor, 2).unwrap();
        if page.is_empty() {
            break;
        }
        let last = page.last().unwrap();
        cursor = Some((last.created_at_ms, last.ledger_id));
        collected.extend(page);
    }

    assert_eq!(
        collected.len(),
        9,
        "pagination must visit all 9 items without drops/dups"
    );
    let full = store.scan_probe_obligations(20).unwrap();
    assert_eq!(
        collected, full,
        "paginated sequence must match exact index ordering"
    );
}
