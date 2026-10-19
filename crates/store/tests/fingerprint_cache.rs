use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
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
    let row = LedgerRow {
        id: LedgerId::new(),
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
    };
    store.insert_ledger(&row).unwrap();
    row.id.to_string()
}

fn entry(cache_key: &str) -> FingerprintCacheEntry {
    FingerprintCacheEntry {
        cache_key: cache_key.into(),
        algorithm_version: 1,
        sample_duration_secs: 180,
        media_duration_ms: Some(2_700_000),
        intro: vec![1, 3, 5, 7],
        outro: Some(vec![2, 4, 6, 8]),
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
    assert_eq!(cached.intro, vec![1, 3, 5, 7]);
    assert_eq!(cached.outro, Some(vec![2, 4, 6, 8]));
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
    let store = Store::open(&temp.path().join("data")).unwrap();
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
    let store = Store::open(&temp.path().join("data")).unwrap();
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
