#[path = "support/mod.rs"]
mod support;

use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
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
        path_key: String::new(),
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
        let report = store
            .publish_files(&scope, &[store_pub], 1000)
            .expect("atomic publish");
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

#[tokio::test]
async fn claim_multi_episode_preserves_all_units() {
    let fix = PresenceFixture::new();
    let store_arc = fix.state.store();
    let (tv_lib, root_id) = {
        let store = store_arc.lock();
        let lib = store
            .create_library(
                MediaKind::Tv,
                "TV Shows 2",
                &["/tv_shows_2"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&lib.id))
            .map(|r| r.id)
            .unwrap();
        (lib, root)
    };

    let lib_id: LibraryId = tv_lib.id.parse().unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Test Multi Episode".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    {
        let store = store_arc.lock();
        store.insert_media(&media).unwrap();
    }

    let file_id = LedgerId::new();
    let multi_coverage = FileCoverage {
        path_key: String::new(),
        file: LibraryFile {
            file_id,
            library_id: lib_id,
            root_id,
            media_id,
            kind: MediaKind::Tv,
            path: PathBuf::from("Test Multi Episode/S01E01-E02.mkv"),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Pending,
            size_bytes: Some(1024 * 1024),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: Some(PathBuf::from("/downloads/source_multi.mkv")),
            transfer_mode: Some("hardlink".into()),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::new(1, 1), UnitKey::new(1, 2)],
    };

    {
        let store = store_arc.lock();
        let reserved = store
            .reserve_transfer(
                &scope,
                &multi_coverage,
                std::path::Path::new("/downloads/source_multi.mkv"),
                library::TransferMode::Hardlink,
                1_000,
            )
            .unwrap();

        store
            .finish_transfer(&scope, &reserved, &reserved.file.version, 2_000)
            .unwrap();

        let units = store.get_file_units(file_id).unwrap();
        assert_eq!(units, vec![UnitKey::new(1, 1), UnitKey::new(1, 2)]);

        let item = store.get_library_item(lib_id, media_id).unwrap().unwrap();
        assert_eq!(item.distinct_episode_count, 2);
        assert_eq!(item.file_count, 1);
        assert_eq!(item.size_bytes, Some(1024 * 1024));
    }
}

#[tokio::test]
async fn replacement_scan_advances_version_and_rejects_old_probe() {
    let fix = PresenceFixture::new();
    let store_arc = fix.state.store();
    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Test Movie 2".into(),
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

    let file_id = LedgerId::new();
    let rel_path = std::path::PathBuf::from("Test Movie 2/movie.mkv");
    let before = FileVersion {
        revision: 1,
        source_version: "stat:100:1000".into(),
    };
    let file = LibraryFile {
        file_id,
        library_id: lib_id,
        root_id,
        media_id,
        kind: MediaKind::Movie,
        path: rel_path,
        version: before.clone(),
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(100),
        stat_known: true,
        created_ns: None,
        modified_ns: Some(1000),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("720p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: Some(100),
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };

    {
        let store = store_arc.lock();
        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    // Now observe a replaced source B
    let observation = library::SourceObservation {
        source_version: "stat:200:2000".into(),
        size_bytes: Some(200),
        modified_ns: Some(2000),
        created_ns: None,
        content_digest: None,
    };

    let (after, old_probe_published, stored_details) = {
        let store = store_arc.lock();
        let after = store
            .replace_file_source(&scope, file_id, &before, &observation, 2000)
            .expect("source replaced");

        // Old probe for `before` version
        let old_probe = store::library_write::probe::ProbePublication {
            expected: &before,
            tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
            resolution: Some("1080p"),
            codec: Some("h264"),
            hdr: None,
            duration_ms: Some(10000),
            details: None,
        };
        let old_probe_published = store
            .publish_probe_result(file_id, &old_probe, 2500)
            .unwrap();

        // New probe for `after` version
        let new_probe = store::library_write::probe::ProbePublication {
            expected: &after,
            tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
            resolution: Some("2160p"),
            codec: Some("hevc"),
            hdr: None,
            duration_ms: Some(20000),
            details: None,
        };
        let new_probe_published = store
            .publish_probe_result(file_id, &new_probe, 3000)
            .unwrap();
        assert!(new_probe_published);

        let stored_file = store.get_file_version(file_id).unwrap().unwrap();
        (after, old_probe_published, stored_file)
    };

    assert_eq!(after.revision, before.revision + 1);
    assert_ne!(after.source_version, before.source_version);
    assert!(!old_probe_published);
    assert_eq!(stored_details.source_version, after.source_version);
}

#[tokio::test]
async fn strm_same_stat_changed_url_invalidates_resume() {
    let fix = PresenceFixture::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let strm_path = temp_dir.path().join("test.strm");
    fs::write(&strm_path, "http://example.com/video1").unwrap();

    let obs1 = library::observe_source(&strm_path).unwrap();
    assert!(obs1.source_version.starts_with("strm:"));

    // Change URL in STRM
    fs::write(&strm_path, "http://example.com/video2").unwrap();
    let obs2 = library::observe_source(&strm_path).unwrap();

    assert_ne!(obs1.source_version, obs2.source_version);
    assert_ne!(obs1.content_digest, obs2.content_digest);
}

#[tokio::test]
async fn unknown_root_does_not_publish_missing() {
    let fix = PresenceFixture::new();
    let store_arc = fix.state.store();
    let lib_id: LibraryId = fix.library_id.parse().unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };
    // Non-existent file/root should not prune on unknown/IO failure
    let report = {
        let store = store_arc.lock();
        store
            .apply_presence(&scope, &[], store::library_write::PresenceMode::Prune, 1000)
            .unwrap()
    };
    assert_eq!(report.deleted, 0);
}

/// R01: a real POST scan over a replaced source (STRM A → B with identical stat)
/// advances the FileVersion through the production scan writer; an old Probe
/// pinned to the previous version is then refused.
#[tokio::test]
async fn actual_rescan_replaces_physical_source() {
    let fix = PresenceFixture::new();
    let dir = fix.root.join("Film (2020)");
    fs::create_dir_all(&dir).unwrap();
    let strm = dir.join("Film.2020.strm");
    fs::write(&strm, "http://example.com/aaaa").unwrap();
    let mtime = fs::metadata(&strm).unwrap().modified().unwrap();
    fix.scan_and_wait_completed(&fix.library_id).await;
    let store_arc = fix.state.store();
    let (file_id, before) = {
        let store = store_arc.lock();
        let file = store
            .file_coverage_by_path(&strm.display().to_string())
            .unwrap()
            .expect("scanned row")
            .file;
        (
            file.file_id,
            store.get_file_version(file.file_id).unwrap().unwrap(),
        )
    };

    fs::write(&strm, "http://example.com/bbbb").unwrap();
    fs::File::options()
        .write(true)
        .open(&strm)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    fix.scan_and_wait_completed(&fix.library_id).await;

    let store = store_arc.lock();
    let after = store.get_file_version(file_id).unwrap().unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_ne!(after.source_version, before.source_version);
    let old_probe = store::library_write::probe::ProbePublication {
        expected: &before,
        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
        resolution: Some("1080p"),
        codec: None,
        hdr: None,
        duration_ms: Some(1),
        details: None,
    };
    assert!(
        !store
            .publish_probe_result(file_id, &old_probe, 9_000)
            .unwrap()
    );
}
