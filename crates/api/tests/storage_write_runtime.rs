#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::sync::Arc;
use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId,
    LibraryFile, LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn scan_and_transfer_publication_atomic_file_writes() {
    let fix = PresenceFixture::new();
    let store_arc = fix.state.store();
    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let lib_id: LibraryId = fix.library_id.parse().unwrap();
    let root_id = {
        let store = store_arc.lock();
        store.insert_media(&media).unwrap();
        let roots = store.list_library_roots().unwrap();
        roots
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&fix.library_id))
            .map(|r| r.id)
            .unwrap_or_else(|| "test_root".into())
    };

    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };

    // 1. Multi-episode publication with atomic file coverage
    let file_id = LedgerId::new();
    let rel_path = std::path::PathBuf::from("Test Movie/Test Movie.mkv");
    let file = LibraryFile {
        file_id,
        library_id: lib_id,
        root_id: root_id.clone(),
        media_id,
        kind: MediaKind::Movie,
        path: rel_path.clone(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1024 * 1024),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: Some("copy".into()),
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: None,
        duration_ms: Some(3600_000),
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };

    let units = vec![UnitKey::WHOLE];
    let coverage = FileCoverage {
        file: file.clone(),
        units: units.clone(),
    };

    let store_pub = StorePublication {
        file: &coverage.file,
        units: &coverage.units,
        expected_version: None,
        nfo_observation: None,
        probe_required: true,
    };

    {
        let store = store_arc.lock();
        let report = store.publish_files(&scope, &[store_pub], 1000).expect("atomic publish");
        assert_eq!(report.inserted, 1);
        assert_eq!(report.updated, 0);

        // Verify summary aggregated
        let item = store
            .get_library_item(lib_id, media_id)
            .unwrap()
            .expect("library item must exist");
        assert_eq!(item.file_count, 1);
        assert_eq!(item.available_file_count, 1);
        assert_eq!(item.distinct_episode_count, 0);
        assert_eq!(item.distinct_season_count, 0);
        // Verify probe obligation created with file_id
        assert_eq!(store.count_probe_obligations().unwrap(), 1);
    }
}
