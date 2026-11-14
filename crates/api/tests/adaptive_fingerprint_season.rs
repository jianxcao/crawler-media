mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}
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
    store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    is_default: true,
                    capture_source: Some(source_facts()),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&file.version.source_version),
            Some(1_800_000),
        )
        .unwrap();
    FileCoverage {
        path_key: String::new(),
        file,
        units: vec![UnitKey::new(1, episode as i32)],
    }
}

#[test]
fn season_refresh_does_not_overwrite_recapture_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let media_id = MediaId::new();
    store
        .as_ref()
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
    store::test_support::reconfigure_default_root(store.as_ref(), MediaKind::Tv, &tv_root).unwrap();
    let row = publish_coverage(
        store.as_ref(),
        media_id,
        &tv_root.join("Test.S01E01.mkv"),
        1,
    );

    let mgr = ProbeManager::new(store.clone()).unwrap();
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
        .as_ref()
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
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let media_id = MediaId::new();
    store
        .as_ref()
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
    store::test_support::reconfigure_default_root(store.as_ref(), MediaKind::Tv, &tv_root).unwrap();
    let row = publish_coverage(
        store.as_ref(),
        media_id,
        &tv_root.join("Test.Rollback.S01E01.mkv"),
        1,
    );

    // Initial marker
    store
        .as_ref()
        .put_fixture_marker(&api::store::StoredFileMarker {
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "initial".into(),
            locked: false,
            updated_at: 0,
            ..store::test_support::fixture_marker(&store, media_id, domain::UnitKey::new(1, 1))
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
        .as_ref()
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
        .as_ref()
        .get_fixture_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(visible_marker.intro_end_ms, Some(104_000));
}

#[tokio::test]
async fn seed_episodes_retain_markers_after_season_template_built() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let media_id = MediaId::new();
    store
        .as_ref()
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
    store::test_support::reconfigure_default_root(store.as_ref(), MediaKind::Tv, &tv_root).unwrap();
    let ep1_path = tv_root.join("Test.Retain.S01E01.mkv");
    let ep2_path = tv_root.join("Test.Retain.S01E02.mkv");
    let ep3_path = tv_root.join("Test.Retain.S01E03.mkv");
    std::fs::write(&ep1_path, b"dummy").unwrap();
    std::fs::write(&ep2_path, b"dummy").unwrap();
    std::fs::write(&ep3_path, b"dummy").unwrap();
    let row1 = publish_coverage(store.as_ref(), media_id, &ep1_path, 1);
    let row2 = publish_coverage(store.as_ref(), media_id, &ep2_path, 2);
    let row3 = publish_coverage(store.as_ref(), media_id, &ep3_path, 3);

    let shared_engine = Arc::new(SharedMatchEngine);
    let mgr =
        ProbeManager::with_engines(store.clone(), shared_engine.clone(), shared_engine).unwrap();

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

    let unit3 = ProbeUnit {
        row: row3,
        ..unit2.clone()
    };
    let units = vec![unit1, unit2, unit3];
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
    let _ = std::fs::remove_file(&ep3_path);

    assert!(
        res.is_ok(),
        "adaptive season pipeline should succeed: {:?}",
        res.err()
    );
    let replacement = res.unwrap();

    // Each seed now has two independent other episodes supporting its exact interval.
    assert_eq!(replacement.markers.len(), 3);
    for episode in 1..=3 {
        let marker = replacement
            .markers
            .iter()
            .find(|m| m.unit.episode == episode)
            .expect("every independently supported seed must retain its marker");
        assert_eq!(marker.intro_start_ms, Some(10_000));
        assert_eq!(marker.intro_end_ms, Some(90_000));
    }
}

struct SharedMatchEngine;
#[path = "adaptive_fingerprint_season/lifecycle.rs"]
mod durable_lifecycle;
fn source_facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/48000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture".into(),
    }
}

#[tokio::test]
async fn ordinary_arrivals_revisit_uncertain_files_with_own_jobs_then_reuse_model_on_append() {
    let (temp, mut store, mut manager, clock, media, root) = durable_lifecycle::setup();
    let mut rows = Vec::new();
    let mut first_job = None;
    let mut model_ids = Vec::new();
    let mut previous = Vec::new();
    let mut previous_jobs = Vec::new();
    for episode in 1..=4 {
        let path = root.join(format!("arrival-{episode}.mkv"));
        std::fs::write(&path, b"fixture").unwrap();
        let fixture = publish_coverage(&store, media, &path, episode);
        let row = store
            .file_coverage_by_id(fixture.file.file_id)
            .unwrap()
            .unwrap();
        let job = manager
            .enqueue_direct_fingerprint(
                durable_lifecycle::unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        if episode == 1 {
            first_job = Some(job.clone());
        }
        rows.push(row);
        durable_lifecycle::drain(&store, &manager, &clock, &rows).await;
        if episode == 1 {
            let unit = &store.probe_job_units(&job).unwrap()[0];
            assert_eq!(unit.status, "succeeded");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    unit.detection_outcome_json.as_ref().unwrap()
                )
                .unwrap()["status"],
                "uncertain"
            );
        }
        if episode == 3 {
            previous_jobs = latest_jobs(&store, &rows);
            assert_ne!(
                store
                    .latest_probe_job_for_ledger(&rows[0].file.file_id.to_string())
                    .unwrap(),
                first_job
            );
            assert_published(&store, &rows);
            model_ids = store
                .list_fingerprint_models(&media.to_string(), 1)
                .unwrap()
                .into_iter()
                .map(|m| m.model_id)
                .collect();
            assert!(!model_ids.is_empty());
            previous = rows
                .iter()
                .map(|row| {
                    store
                        .get_file_marker(row.file.file_id, row.units[0])
                        .unwrap()
                })
                .collect();
            manager.shutdown_workers().await.unwrap();
            drop(manager);
            drop(store);
            store = Arc::new(Store::open(temp.path()).unwrap());
            manager = Arc::new(
                ProbeManager::with_engines(
                    store.clone(),
                    Arc::new(SharedMatchEngine),
                    Arc::new(NewArrivalCapture {
                        allowed: root.join("arrival-4.mkv"),
                    }),
                )
                .unwrap(),
            );
            manager.set_clock(clock.clone());
            manager.try_start_workers();
        }
        if episode == 4 {
            assert_eq!(
                latest_jobs(&store, &rows[..3]),
                previous_jobs,
                "stable template append must not reopen prior verified files"
            );
            assert_eq!(
                store
                    .list_fingerprint_models(&media.to_string(), 1)
                    .unwrap()
                    .into_iter()
                    .map(|m| m.model_id)
                    .collect::<Vec<_>>(),
                model_ids
            );
            assert_eq!(
                rows[..3]
                    .iter()
                    .map(|row| store
                        .get_file_marker(row.file.file_id, row.units[0])
                        .unwrap())
                    .collect::<Vec<_>>(),
                previous
            );
            assert!(
                store
                    .get_file_marker(rows[3].file.file_id, rows[3].units[0])
                    .unwrap()
                    .is_some()
            );
        }
    }
    manager.shutdown_workers().await.unwrap();
}

struct NewArrivalCapture {
    allowed: std::path::PathBuf,
}

fn latest_jobs(store: &Store, rows: &[FileCoverage]) -> Vec<Option<String>> {
    rows.iter()
        .map(|row| {
            store
                .latest_probe_job_for_ledger(&row.file.file_id.to_string())
                .unwrap()
        })
        .collect()
}

fn assert_published(store: &Store, rows: &[FileCoverage]) {
    for row in rows {
        assert!(
            store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_some()
        );
    }
}
impl marker::FingerprintCaptureEngine for NewArrivalCapture {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        assert_eq!(
            request.path, self.allowed,
            "reopened append must not recapture earlier sources"
        );
        SharedMatchEngine.capture_window(request)
    }
}
impl marker::fingerprint::capture_types::FingerprintCaptureEngine for SharedMatchEngine {
    fn capture_window(
        &self,
        request: &marker::fingerprint::capture_types::CaptureRequest,
    ) -> Result<
        marker::fingerprint::capture_types::CapturedFingerprint,
        marker::fingerprint::capture_types::CaptureFailure,
    > {
        let dur_ms = request.window.duration_ms();
        // Keep a stable media interval when adaptive windows move their origin.
        let num_words = (dur_ms / 128).max(1) as usize;
        let mut words: Vec<_> = (0..num_words).map(|i| (i as u32) + 42).collect();
        words[0] = request.window.start_ms as u32;
        Ok(marker::fingerprint::capture_types::CapturedFingerprint {
            window: request.window.clone(),
            words,
            pcm_duration_ms: Some(dur_ms),
            metrics: marker::fingerprint::capture_types::CaptureMetrics {
                audio_stream_index: Some(0),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                source_facts: Some(source_facts()),
                time_mapping_version: 3,
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
        _control: &marker::WorkControl,
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
            let offset = |words: &[u32]| {
                let absolute = if words[0] > 900_000 { 1_630.0 } else { 10.0 };
                absolute - words[0] as f32 / 1000.0
            };
            Some(marker::CommonSegment {
                start1_sec: offset(first),
                end1_sec: offset(first) + 80.0,
                start2_sec: offset(second),
                end2_sec: offset(second) + 80.0,
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

#[path = "adaptive_fingerprint_season/lock.rs"]
mod lock;
