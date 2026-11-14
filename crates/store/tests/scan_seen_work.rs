use domain::{Media, MediaId, MediaKind};
use store::{Store, library_write::WriteScope, stats::StatsScope, test_support::FixtureFile};

fn fixture() -> (tempfile::TempDir, Store, std::path::PathBuf, MediaId) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Movie".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    (tmp, store, root, media_id)
}

#[test]
fn unchanged_scan_stamps_use_less_work_than_changed_stamps() {
    let (_tmp, store, root, media_id) = fixture();
    let stamps: Vec<_> = (0..100)
        .map(|i| {
            let path = root.join(format!("{i:03}.mkv"));
            store::test_support::publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            (
                path.display().to_string(),
                10_i64,
                5_i64,
                (i % 2 == 0).then(|| "old".to_string()),
            )
        })
        .collect();
    store.record_scan_seen_stamps("initial", &stamps).unwrap();
    let (same_result, same) = store.with_stats(StatsScope::Library, || {
        store.record_scan_seen_stamps("same", &stamps)
    });
    same_result.unwrap();
    let paths: Vec<_> = stamps.iter().map(|stamp| stamp.0.clone()).collect();
    let checkpoints = store.scan_seen_checkpoints("same", &paths).unwrap();
    let versions = store.ledger_source_versions(&paths).unwrap();
    for (path, mtime, size, digest) in &stamps {
        assert_eq!(checkpoints[path], (*mtime, *size, digest.clone(), true));
        assert_eq!(versions[path], (*mtime, *size, digest.clone()));
    }
    let changed: Vec<_> = stamps
        .iter()
        .map(|(path, mtime, size, digest)| {
            (
                path.clone(),
                mtime + 1,
                size + 1,
                digest.as_ref().map(|_| "new".to_string()),
            )
        })
        .collect();
    let (changed_result, changed_work) = store.with_stats(StatsScope::Library, || {
        store.record_scan_seen_stamps("changed", &changed)
    });
    changed_result.unwrap();
    let checkpoints = store.scan_seen_checkpoints("changed", &paths).unwrap();
    let versions = store.ledger_source_versions(&paths).unwrap();
    for (path, mtime, size, digest) in &changed {
        assert_eq!(checkpoints[path], (*mtime, *size, digest.clone(), true));
        assert_eq!(versions[path], (*mtime, *size, digest.clone()));
    }
    println!(
        "scan stamp SQL VM work: unchanged={}, changed={}",
        same.vm_steps, changed_work.vm_steps
    );
    assert!(
        same.vm_steps < changed_work.vm_steps,
        "unchanged scan should use less SQL VM work: unchanged={}, changed={}",
        same.vm_steps,
        changed_work.vm_steps
    );
}

#[test]
fn scan_stamp_updates_each_source_field_and_survives_reopen() {
    let (tmp, store, root, media_id) = fixture();
    let path = root.join("movie.strm").display().to_string();
    let file = store::test_support::publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: path.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    let scope = WriteScope {
        library_id: file.library_id,
        config_revision: store
            .library_config_revision(&file.library_id.to_string())
            .unwrap(),
    };
    store
        .replace_file_source(
            &scope,
            file.file_id,
            &file.version,
            &library::SourceObservation {
                source_version: "fractional".into(),
                size_bytes: Some(5),
                modified_ns: Some(10_000_123),
                created_ns: None,
                content_digest: None,
            },
            1,
        )
        .unwrap();
    let paths = vec![path.clone()];
    assert_eq!(
        store
            .file_coverage_by_path(&path)
            .unwrap()
            .unwrap()
            .file
            .modified_ns,
        Some(10_000_123)
    );
    for (index, (mtime, size, digest)) in [
        (10, 5, None),
        (11, 5, None),
        (11, 6, None),
        (11, 6, Some("a")),
        (11, 6, Some("b")),
        (11, 6, None),
    ]
    .into_iter()
    .enumerate()
    {
        let digest = digest.map(str::to_string);
        let op = format!("operation-{index}");
        store
            .record_scan_seen_stamps(&op, &[(path.clone(), mtime, size, digest.clone())])
            .unwrap();
        assert_eq!(
            store.scan_seen_checkpoints(&op, &paths).unwrap()[&path],
            (mtime, size, digest.clone(), true)
        );
        assert_eq!(
            store.ledger_source_versions(&paths).unwrap()[&path],
            (mtime, size, digest)
        );
        assert_eq!(
            store
                .file_coverage_by_path(&path)
                .unwrap()
                .unwrap()
                .file
                .modified_ns,
            Some(mtime * 1_000_000)
        );
    }
    drop(store);
    let reopened = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        reopened.ledger_source_versions(&paths).unwrap()[&path],
        (11, 6, None)
    );
    assert_eq!(
        reopened
            .scan_seen_checkpoints("operation-5", &paths)
            .unwrap()[&path],
        (11, 6, None, true)
    );
}

#[test]
fn failed_scan_stamp_batch_rolls_back_checkpoints_and_versions() {
    let (_tmp, store, root, media_id) = fixture();
    let stamps: Vec<_> = (0..100)
        .map(|i| {
            let path = root.join(format!("{i:03}.mkv")).display().to_string();
            store::test_support::publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id,
                    path: path.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
            (path, 10, 5, None)
        })
        .collect();
    store.record_scan_seen_stamps("initial", &stamps).unwrap();
    let mut changed: Vec<_> = stamps
        .iter()
        .map(|(path, _, _, _)| (path.clone(), 11, 6, None))
        .collect();
    changed.last_mut().unwrap().2 = -1;
    let result = store.record_scan_seen_stamps("interrupted", &changed);
    assert!(result.is_err());
    let paths: Vec<_> = stamps.iter().map(|stamp| stamp.0.clone()).collect();
    assert!(
        store
            .scan_seen_checkpoints("interrupted", &paths)
            .unwrap()
            .is_empty()
    );
    let versions = store.ledger_source_versions(&paths).unwrap();
    for path in paths {
        assert_eq!(versions[&path], (10, 5, None));
    }
}
