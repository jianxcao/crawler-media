use store::{FingerprintCacheEntry, Store};

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
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            duration_secs: Some(2_700.0),
            ..library::VideoTrack::default()
        }),
        audio: Vec::new(),
        subtitles: Vec::new(),
    };

    store
        .put_file_meta_versioned(
            "episode-ledger",
            &tracks,
            Some("source-version"),
            Some(2_700_000),
        )
        .unwrap();

    assert_eq!(store.get_file_meta("episode-ledger").unwrap(), Some(tracks));
    assert_eq!(
        store
            .get_media_info_cache_version("episode-ledger")
            .unwrap()
            .unwrap(),
        store::MediaInfoCacheVersion {
            source_version: "source-version".into(),
            format_duration_ms: Some(2_700_000),
        }
    );
}
