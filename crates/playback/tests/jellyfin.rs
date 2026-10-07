use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use playback::{Item, media_source, parse_range};

fn row() -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id: MediaId::new(),
        path: "/library/matrix.mkv".into(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
    }
}

#[test]
fn media_source_is_direct_play_only() {
    let source = media_source(&row());
    assert_eq!(source.supports_direct_play, true);
    assert_eq!(source.supports_transcoding, false);
    assert_eq!(source.path, "/library/matrix.mkv");
}

#[test]
fn item_id_is_ledger_id_without_hyphens() {
    let item = Item::from_ledger("The Matrix", &row());
    assert!(!item.id.contains('-'));
    assert_eq!(item.name, "The Matrix");
}

#[test]
fn parse_range_header_is_inclusive() {
    let spec = parse_range("bytes=0-99", 1000).unwrap();
    assert_eq!(spec.start, 0);
    assert_eq!(spec.end, 99);
    assert_eq!(spec.len(), 100);
}
