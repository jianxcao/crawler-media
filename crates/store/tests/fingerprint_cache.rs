use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use store::test_support::FixtureFile;
use store::{FingerprintCacheEntry, Store};

/// `file_details` has a foreign key to `library_files`, so a details fixture
/// needs a real published file row.
fn seed_file(store: &Store, root: &std::path::Path, title: &str) -> String {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            &format!("lib-{title}"),
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let row = FixtureFile {
        file_id: Some(LedgerId::new()),
        media_id: media.id,
        path: root.join(format!("{title}.mkv")).display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &row).unwrap();
    row.file_id.expect("fixture file id").to_string()
}

fn entry(cache_key: &str) -> FingerprintCacheEntry {
    FingerprintCacheEntry {
        cache_key: cache_key.into(),
        algorithm_version: 1,
        sample_duration_secs: 180,
        media_duration_ms: Some(2_700_000),
        intro: marker::CapturedFingerprint {
            window: marker::SampleWindow::new(0, 180_000),
            words: vec![1, 3, 5, 7],
            pcm_duration_ms: Some(180_000),
            metrics: marker::CaptureMetrics::default(),
        },
        outro: Some(marker::CapturedFingerprint {
            window: marker::SampleWindow::new(2_520_000, 2_700_000),
            words: vec![2, 4, 6, 8],
            pcm_duration_ms: Some(180_000),
            metrics: marker::CaptureMetrics::default(),
        }),
    }
}

#[test]
fn fingerprint_cache_survives_reopen_and_can_be_invalidated_by_version() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let ledger_id = "episode-ledger";

    let store = Store::open(&data).unwrap();
    store
        .put_fingerprint_cache(ledger_id, &entry("source-v1-profile-v1"))
        .unwrap();
    drop(store);

    let store = Store::open(&data).unwrap();
    let cached = store.get_fingerprint_cache(ledger_id).unwrap().unwrap();
    assert_eq!(cached.cache_key, "source-v1-profile-v1");
    assert_eq!(cached.intro.words, vec![1, 3, 5, 7]);
    assert_eq!(cached.outro.unwrap().words, vec![2, 4, 6, 8]);
    assert_eq!(cached.media_duration_ms, Some(2_700_000));

    store
        .put_fingerprint_cache(ledger_id, &entry("source-v2-profile-v1"))
        .unwrap();
    assert_eq!(
        store
            .get_fingerprint_cache(ledger_id)
            .unwrap()
            .unwrap()
            .cache_key,
        "source-v2-profile-v1"
    );
}

#[test]
fn deleting_file_metadata_also_deletes_its_fingerprint_cache() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    store
        .put_fingerprint_cache("episode-ledger", &entry("source-v1-profile-v1"))
        .unwrap();

    store
        .delete_file_meta_by_ledger_id("episode-ledger")
        .unwrap();

    assert!(
        store
            .get_fingerprint_cache("episode-ledger")
            .unwrap()
            .is_none()
    );
}

#[test]
fn stream_duration_and_source_version_are_persisted_with_media_information() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let file_id = seed_file(&store, &temp.path().join("media"), "details");
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            duration_secs: Some(2_700.0),
            ..library::VideoTrack::default()
        }),
        audio: Vec::new(),
        subtitles: Vec::new(),
    };

    store
        .put_file_meta_versioned(&file_id, &tracks, None, Some(2_700_000))
        .unwrap();

    assert_eq!(store.get_file_meta(&file_id).unwrap(), Some(tracks));
    let parsed: LedgerId = file_id.parse().unwrap();
    let source_version = store
        .file_details_batch(&[parsed])
        .unwrap()
        .remove(&parsed)
        .unwrap()
        .source_version;
    assert_eq!(
        store
            .get_media_info_cache_version(&file_id)
            .unwrap()
            .unwrap(),
        store::MediaInfoCacheVersion {
            source_version,
            format_duration_ms: Some(2_700_000),
        }
    );
}

#[test]
fn full_capture_cache_roundtrip_preserves_rms_diagnostics_and_actual_windows() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut cache = entry("source-profile-mapping");
    cache.intro.metrics.pcm_frame_ms = 20;
    cache.intro.metrics.pcm_rms_dbfs_milli = (0..9_000).map(|n| -20_000 - n).collect();
    cache.intro.metrics.pcm_timeline = marker::PcmTimeline::Contiguous;
    cache.intro.metrics.contiguous_pcm_start_ms = Some(0);
    cache.intro.metrics.ffmpeg_stderr_tail = Some("same capture diagnostics".into());
    cache.outro.as_mut().unwrap().metrics = cache.intro.metrics.clone();
    store.put_fingerprint_cache("capture", &cache).unwrap();
    assert_eq!(store.get_fingerprint_cache("capture").unwrap(), Some(cache));
}

#[test]
fn owned_capture_columns_outlive_snapshot_and_prepared_publication_is_immutable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let id = seed_file(&store, &temp.path().join("media"), "owned-cache");
    let file = store
        .file_coverage_by_id(id.parse().unwrap())
        .unwrap()
        .unwrap()
        .file;
    let before = entry("before");
    store.put_fingerprint_cache(&id, &before).unwrap();
    let raw = store
        .with_read_snapshot(
            &[store::Db::Library],
            std::time::Duration::from_secs(1),
            || store.read_fingerprint_cache(&id).unwrap().unwrap(),
        )
        .unwrap();
    let mut next = entry("prepared");
    let prepared = next.prepare().unwrap();
    next.intro.words = vec![999];
    store
        .library_write(|db| db.put_fingerprint_cache_checked(&id, Some(&file.version), &prepared))
        .unwrap();
    assert_eq!(
        raw.decode().unwrap(),
        before,
        "owned raw payload is the previous coherent snapshot"
    );
    assert_eq!(
        store.get_fingerprint_cache(&id).unwrap(),
        Some(entry("prepared")),
        "publication binds immutable preencoded captures"
    );
}
