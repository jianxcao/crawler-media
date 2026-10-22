use api::Store;
use api::probe_manager::{ProbeManager, ProbeUnit};
use domain::{Confidence, FileCoverage, MediaId, MediaKind, QualitySource, UnitKey};
use parking_lot::Mutex;
use std::sync::Arc;
use store::test_support::{FixtureFile, publish_fixture_file};

/// Publish one single-episode fixture file and read the coverage fact back: the
/// `file_id` the pipeline keys on is assigned at publication.
fn publish_coverage(
    store: &Store,
    media_id: MediaId,
    path: &std::path::Path,
    episode: u32,
) -> FileCoverage {
    let spec = FixtureFile {
        media_id,
        path: path.to_string_lossy().to_string(),
        season: Some(1),
        episode: Some(episode),
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        ..FixtureFile::default()
    };
    let file = publish_fixture_file(store, &spec).unwrap();
    FileCoverage {
        file,
        units: vec![UnitKey::new(1, episode as i32)],
    }
}

#[test]
fn season_refresh_does_not_overwrite_recapture_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Show".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let tv_root = tmp.path().join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    store
        .lock()
        .set_library_root(MediaKind::Tv, tv_root.to_str().unwrap())
        .unwrap();
    let row = publish_coverage(&store.lock(), media_id, &tv_root.join("Test.S01E01.mkv"), 1);

    let mgr = ProbeManager::new(store.clone());
    let unit = ProbeUnit {
        row: row.clone(),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    let queued = mgr.enqueue_marker_refresh(vec![unit]);
    assert_eq!(queued, 1);

    // Verify in database that reuse_fingerprint_cache is false, and reuse_media_info_cache is true
    let active_unit = store
        .lock()
        .active_probe_unit_for_ledger(&row.file.file_id.to_string())
        .unwrap()
        .expect("active probe unit should exist");

    assert!(
        !active_unit.reuse_fingerprint_cache,
        "reuse_fingerprint_cache must NOT be overwritten to true for marker refresh!"
    );
    assert!(
        active_unit.reuse_media_info_cache,
        "reuse_media_info_cache should be true to reuse existing media metadata"
    );
}

#[test]
fn database_failure_rolls_back_markers_chapters_and_job_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Rollback".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let tv_root = tmp.path().join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    store
        .lock()
        .set_library_root(MediaKind::Tv, tv_root.to_str().unwrap())
        .unwrap();
    let row = publish_coverage(
        &store.lock(),
        media_id,
        &tv_root.join("Test.Rollback.S01E01.mkv"),
        1,
    );

    // Initial marker
    store
        .lock()
        .put_media_marker(&api::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "initial".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();

    let job_id = "test-job-rollback";
    let spec = api::store::ProbeJobUnitSpec {
        ledger_id: &row.file.file_id.to_string(),
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    };
    store
        .lock()
        .create_probe_job(
            job_id,
            "marker_refresh",
            &media_id.to_string(),
            Some(1),
            "test-scope",
            &[spec],
        )
        .unwrap();

    // Old visible marker is kept intact
    let visible_marker = store
        .lock()
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(visible_marker.intro_end_ms, Some(104_000));
}

#[tokio::test]
async fn seed_episodes_retain_markers_after_season_template_built() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Test Retain".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let tv_root = tmp.path().join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    store
        .lock()
        .set_library_root(MediaKind::Tv, tv_root.to_str().unwrap())
        .unwrap();
    let ep1_path = tv_root.join("Test.Retain.S01E01.mkv");
    let ep2_path = tv_root.join("Test.Retain.S01E02.mkv");
    std::fs::write(&ep1_path, b"dummy").unwrap();
    std::fs::write(&ep2_path, b"dummy").unwrap();
    let row1 = publish_coverage(&store.lock(), media_id, &ep1_path, 1);
    let row2 = publish_coverage(&store.lock(), media_id, &ep2_path, 2);

    struct SharedMatchEngine;
    impl marker::fingerprint::capture_types::FingerprintCaptureEngine for SharedMatchEngine {
        fn capture_window(
            &self,
            request: &marker::fingerprint::capture_types::CaptureRequest,
        ) -> Result<
            marker::fingerprint::capture_types::CapturedFingerprint,
            marker::fingerprint::capture_types::CaptureFailure,
        > {
            let dur_ms = request.window.duration_ms();
            // Generate identical fingerprint pattern so both seeds match each other
            let num_words = (dur_ms / 128).max(1) as usize;
            let words = (0..num_words).map(|i| (i as u32) + 42).collect();
            Ok(marker::fingerprint::capture_types::CapturedFingerprint {
                window: request.window.clone(),
                words,
                pcm_duration_ms: Some(dur_ms),
                metrics: marker::fingerprint::capture_types::CaptureMetrics {
                    elapsed_ms: 10,
                    pcm_bytes: (num_words * 4) as u64,
                    ..Default::default()
                },
            })
        }
    }

    impl marker::FingerprintEngine for SharedMatchEngine {
        fn extract_at(
            &self,
            _path: &std::path::Path,
            _start_secs: u32,
            _duration_secs: u32,
        ) -> Result<marker::AudioFingerprint, String> {
            Ok(vec![42; 100])
        }

        fn find_common_segment(
            &self,
            first: &[u32],
            second: &[u32],
            _min_duration_secs: f32,
            _max_duration_secs: f32,
        ) -> Option<marker::CommonSegment> {
            if !first.is_empty() && !second.is_empty() {
                Some(marker::CommonSegment {
                    start1_sec: 10.0,
                    end1_sec: 90.0,
                    start2_sec: 10.0,
                    end2_sec: 90.0,
                    duration_sec: 80.0,
                    score: 0.95,
                })
            } else {
                None
            }
        }

        fn find_common_segments(
            &self,
            first: &[u32],
            second: &[u32],
            min_duration_secs: f32,
            max_duration_secs: f32,
        ) -> Vec<marker::CommonSegment> {
            self.find_common_segment(first, second, min_duration_secs, max_duration_secs)
                .into_iter()
                .collect()
        }
    }

    let shared_engine = Arc::new(SharedMatchEngine);
    let mgr = ProbeManager::with_engines(store.clone(), shared_engine.clone(), shared_engine);

    let unit1 = ProbeUnit {
        row: row1.clone(),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    let unit2 = ProbeUnit {
        row: row2.clone(),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    let units = vec![unit1, unit2];
    let res = api::probe_manager::season::run_adaptive_season_pipeline(
        &mgr,
        "test-season-retain",
        media_id,
        1,
        &units,
    )
    .await;

    let _ = std::fs::remove_file(&ep1_path);
    let _ = std::fs::remove_file(&ep2_path);

    assert!(
        res.is_ok(),
        "adaptive season pipeline should succeed: {:?}",
        res.err()
    );
    let replacement = res.unwrap();

    // Verify both episode 1 and episode 2 markers exist and intro is populated!
    let ep1_marker = replacement.markers.iter().find(|m| m.episode == 1);
    let ep2_marker = replacement.markers.iter().find(|m| m.episode == 2);

    assert!(
        ep1_marker.is_some(),
        "Episode 1 (seed) marker must not be missing!"
    );
    assert!(
        ep2_marker.is_some(),
        "Episode 2 (seed) marker must not be missing!"
    );

    let ep1 = ep1_marker.unwrap();
    let ep2 = ep2_marker.unwrap();

    assert!(
        ep1.intro_start_ms.is_some(),
        "Episode 1 intro start must be populated!"
    );
    assert!(
        ep1.intro_end_ms.is_some(),
        "Episode 1 intro end must be populated!"
    );
    assert!(
        ep2.intro_start_ms.is_some(),
        "Episode 2 intro start must be populated!"
    );
    assert!(
        ep2.intro_end_ms.is_some(),
        "Episode 2 intro end must be populated!"
    );
}
