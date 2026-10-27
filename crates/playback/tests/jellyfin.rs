use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    MediaId, MediaKind, QualitySource, TypedItemId, UnitKey,
};
use playback::{Item, media_source, parse_range};
use std::path::PathBuf;

fn sample_file() -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id: LibraryId::new(),
        root_id: "root1".into(),
        media_id: MediaId::new(),
        kind: MediaKind::Movie,
        path: PathBuf::from("/library/matrix.mkv"),
        version: FileVersion {
            revision: 3,
            source_version: "v3".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1024 * 1024),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: None,
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

#[test]
fn media_source_is_direct_play_only() {
    let file = sample_file();
    let source = media_source(&file);
    assert!(source.supports_direct_play);
    assert!(!source.supports_transcoding);
    assert_eq!(source.path, "/library/matrix.mkv");
    assert!(source.id.starts_with("f1_"));
}

#[test]
fn item_from_target_and_shared_file_different_item_ids() {
    let file = sample_file();
    let media_id = file.media_id;

    let ep1_id = TypedItemId::Unit {
        media: media_id,
        key: UnitKey::new(1, 1),
    };
    let ep2_id = TypedItemId::Unit {
        media: media_id,
        key: UnitKey::new(1, 2),
    };

    let item1 = Item::from_target(&ep1_id, "Episode 1", &file);
    let item2 = Item::from_target(&ep2_id, "Episode 2", &file);

    // Both items share the exact same physical media source
    let source1 = media_source(&file);
    let source2 = media_source(&file);
    assert_eq!(source1.id, source2.id);
    assert!(source1.id.starts_with("f1_"));

    // But items have distinct logical IDs
    assert_ne!(item1.id, item2.id);
    assert!(item1.id.starts_with("u1_"));
    assert!(item2.id.starts_with("u1_"));
    assert_eq!(item1.name, "Episode 1");
    assert_eq!(item2.name, "Episode 2");
    assert_eq!(item1.path, item2.path);
}
#[test]
fn parse_range_header_is_inclusive() {
    let spec = parse_range("bytes=0-99", 1000).unwrap();
    assert_eq!(spec.start, 0);
    assert_eq!(spec.end, 99);
    assert_eq!(spec.len(), 100);
}
