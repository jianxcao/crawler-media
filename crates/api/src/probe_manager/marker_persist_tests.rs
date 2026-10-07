#[test]
fn fresh_fingerprint_replaces_stale_unlocked_intro_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: tmp.path().join("Pantheon.S01E01.mkv").display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "theintrodb".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();

    persist_marker(
        &store,
        &[row.clone()],
        crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(180_000),
            outro_start_ms: Some(1_000_000),
            outro_end_ms: Some(1_100_000),
            source: "fingerprint".into(),
            locked: false,
            updated_at: 0,
        },
        false,
    )
    .unwrap();

    let marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_end_ms, Some(180_000));
    assert_eq!(marker.outro_start_ms, Some(1_000_000));
    assert_eq!(marker.source, "fingerprint");
}
