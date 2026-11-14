#[path = "probe_retry_support.rs"]
mod support;

use api::probe_manager::clock::ProbeClock;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::probe_manager::{ProbeManager, ProbeUnit};
use domain::{FileCoverage, FileVersion, MediaKind};
use marker::{AudioFingerprint, CommonSegment, FingerprintEngine};
use store::Store;
use support::ProbeScenario;

struct ReplaceSource {
    store: Arc<Store>,
    row: FileCoverage,
    during_capture: bool,
    publish_replacement: bool,
    replaced: AtomicBool,
}

impl ReplaceSource {
    fn replace(&self) {
        if self.replaced.swap(true, Ordering::SeqCst) {
            return;
        }
        std::fs::write(
            &self.row.file.path,
            "https://example.invalid/new-source.mkv\n",
        )
        .unwrap();
        if !self.publish_replacement {
            return;
        }
        let mut file = self.row.file.clone();
        file.version = FileVersion {
            revision: file.version.revision + 1,
            source_version: library::observe_source(&file.path).unwrap().source_version,
        };
        let store = self.store.as_ref();
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: file.library_id,
                    config_revision: store
                        .library_config_revision(&file.library_id.to_string())
                        .unwrap(),
                },
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &self.row.units,
                    expected_version: Some(&self.row.file.version),
                    nfo_observation: None,
                    probe_required: false,
                }],
                2_000,
            )
            .unwrap();
    }
}

impl FingerprintEngine for ReplaceSource {
    fn extract_at(
        &self,
        path: &Path,
        _: u32,
        _: u32,
        _control: &marker::WorkControl,
    ) -> Result<AudioFingerprint, String> {
        if self.during_capture && path == self.row.file.path {
            self.replace();
        }
        Ok(vec![8, 9, 10, 11])
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        if !self.during_capture {
            self.replace();
        }
        // Stable semantics for an exact ordered pair, including across retries.
        let start = if first.first().is_some_and(|word| *word >= 20)
            || second.first().is_some_and(|word| *word >= 20)
        {
            30.0
        } else {
            10.0
        };
        Some(CommonSegment {
            start1_sec: start,
            end1_sec: start + 50.0,
            start2_sec: start,
            end2_sec: start + 50.0,
            duration_sec: 50.0,
            score: 1.0,
        })
    }
}

async fn scenario(
    during_capture: bool,
    publish_replacement: bool,
) -> (ProbeScenario, Arc<ReplaceSource>) {
    let mut scenario = ProbeScenario::cached_season();
    scenario.manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(ReplaceSource {
        store: scenario.store.clone(),
        row: scenario.ledgers[0].clone(),
        during_capture,
        publish_replacement,
        replaced: AtomicBool::new(false),
    });
    scenario.manager = Arc::new(
        ProbeManager::with_engines(scenario.store.clone(), engine.clone(), engine.clone()).unwrap(),
    );
    scenario.manager.set_clock(scenario.clock.clone());
    scenario.manager.try_start_workers();
    (scenario, engine)
}

async fn refresh(scenario: &ProbeScenario) -> store::ProbeJob {
    refresh_rows(scenario, &scenario.ledgers).await
}

async fn refresh_rows(scenario: &ProbeScenario, rows: &[FileCoverage]) -> store::ProbeJob {
    enqueue_refresh(scenario, rows);
    finish_refresh(scenario).await
}

fn enqueue_refresh(scenario: &ProbeScenario, rows: &[FileCoverage]) {
    let units = rows
        .iter()
        .map(|row| ProbeUnit {
            row: row.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    assert_eq!(scenario.manager.enqueue_marker_refresh(units), rows.len());
}

async fn finish_refresh(scenario: &ProbeScenario) -> store::ProbeJob {
    let job = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let job = scenario
                .store
                .as_ref()
                .latest_marker_refresh_for_season(&scenario.media_id.to_string(), 1)
                .unwrap()
                .unwrap();
            if !job.is_active() {
                return job;
            }
            advance_analysis(scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("source race must reach a persisted terminal state");
    scenario.manager.shutdown_workers().await.unwrap();
    job
}

fn advance_analysis(scenario: &ProbeScenario) {
    if !scenario
        .store
        .pending_probe_analysis_keys(None, 1)
        .unwrap()
        .is_empty()
    {
        scenario.clock.set(scenario.clock.now_ms() + 2_000);
        scenario.manager.wake_dispatch();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_comparison_cannot_publish_old_markers() {
    let (scenario, engine) = scenario(false, false).await;
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.as_ref();
    assert!(
        store
            .get_fixture_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .is_none(),
        "marker computed from the previous STRM target must not publish"
    );
    assert!(
        store
            .get_cached_chapters(&engine.row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_eq!(job.status, "succeeded");
    let units = store.probe_job_units(&job.id).unwrap();
    assert_eq!(
        units
            .iter()
            .filter(|unit| unit.status == "cancelled")
            .count(),
        1
    );
    assert_eq!(job.succeeded, 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_change_during_comparison_cannot_publish_old_markers() {
    let (scenario, engine) = scenario(false, true).await;
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.as_ref();
    assert!(
        store
            .get_fixture_marker(scenario.media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_cached_chapters(&engine.row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_ne!(job.status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_refresh_checks_comparison_inputs_outside_its_lease() {
    let (scenario, engine) = scenario(false, false).await;
    enqueue_refresh(&scenario, &scenario.ledgers[1..3]);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !engine.replaced.load(Ordering::SeqCst) {
            advance_analysis(&scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        loop {
            let job = scenario
                .store
                .latest_marker_refresh_for_season(&scenario.media_id.to_string(), 1)
                .unwrap()
                .unwrap();
            if scenario
                .store
                .probe_job_units(&job.id)
                .unwrap()
                .iter()
                .all(|unit| unit.status == "queued")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(engine.replaced.load(Ordering::SeqCst));
    let store = scenario.store.as_ref();
    for episode in [2, 3] {
        assert!(
            store
                .get_fixture_marker(scenario.media_id, Some(1), Some(episode))
                .unwrap()
                .is_none(),
            "the changed unleased sample participated in this comparison"
        );
    }
    let job = finish_refresh(&scenario).await;
    assert_eq!(
        job.status, "succeeded",
        "surviving sources can be recomputed after rejecting the stale attempt"
    );
    assert_eq!(job.succeeded, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_capture_cannot_overwrite_fingerprint_cache() {
    let (scenario, engine) = scenario(true, false).await;
    let file_id = engine.row.file.file_id.to_string();
    let before = scenario
        .store
        .as_ref()
        .get_fingerprint_cache(&file_id)
        .unwrap()
        .unwrap();
    let job = refresh(&scenario).await;
    assert!(engine.replaced.load(Ordering::SeqCst));
    let after = scenario
        .store
        .as_ref()
        .get_fingerprint_cache(&file_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.intro, before.intro,
        "unscanned disk replacement must reject the old capture"
    );
    assert_eq!(job.status, "succeeded");
    assert_eq!(job.succeeded, 7);
    assert_eq!(
        scenario
            .store
            .probe_job_units(&job.id)
            .unwrap()
            .iter()
            .filter(|unit| unit.status == "cancelled")
            .count(),
        1
    );
}

struct ReplacingTracks;

impl api::probe_manager::TracksProbe for ReplacingTracks {
    fn probe(
        &self,
        path: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        std::fs::write(path, "https://example.invalid/replaced-during-tracks.mkv\n")?;
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("hevc".into()),
                    width: Some(3840),
                    height: Some(2160),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(99_000),
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_change_during_tracks_cannot_publish_details_or_nfo() {
    let mut scenario = ProbeScenario::cached_season();
    scenario.manager.shutdown_workers().await.unwrap();
    scenario.manager = Arc::new(
        ProbeManager::with_engines(
            scenario.store.clone(),
            scenario.engine.clone(),
            scenario.engine.clone(),
        )
        .unwrap()
        .with_tracks_probe(Arc::new(ReplacingTracks)),
    );
    let row = scenario.ledgers[0].clone();
    let nfo_path = row.file.path.with_extension("nfo");
    let before_nfo = "<episodedetails><title>Original</title></episodedetails>";
    std::fs::write(&nfo_path, before_nfo).unwrap();
    let file_id = row.file.file_id.to_string();
    assert!(scenario.manager.enqueue_force(ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));
    scenario.manager.try_start_workers();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = scenario
                .store
                .as_ref()
                .latest_probe_job_for_scope(&format!("ledger:{file_id}"))
                .unwrap()
                .unwrap();
            if !job.is_active() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("metadata race must terminate");
    scenario.manager.shutdown_workers().await.unwrap();
    let details = scenario
        .store
        .as_ref()
        .get_file_meta(&file_id)
        .unwrap()
        .unwrap();
    assert_eq!(details.video.unwrap().width, Some(1920));
    assert_eq!(
        std::fs::read_to_string(nfo_path).unwrap(),
        before_nfo,
        "a stale stream probe must not modify NFO sidecars"
    );
}

async fn probe_once(scenario: &ProbeScenario, row: FileCoverage) {
    let id = row.file.file_id.to_string();
    assert!(scenario.manager.enqueue(ProbeUnit {
        row,
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let terminal = scenario
                .store
                .as_ref()
                .latest_probe_job_for_scope(&format!("ledger:{id}"))
                .unwrap()
                .is_some_and(|job| !job.is_active());
            if terminal && !scenario.manager.is_queued(&id) {
                break;
            }
            advance_analysis(scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("probe must finish and release its queue identity");
}

fn replace_with_equal_length_words(
    scenario: &ProbeScenario,
    engine: &ReplaceSource,
) -> FileCoverage {
    let mut before = scenario
        .store
        .as_ref()
        .get_fingerprint_cache(&engine.row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    ReplaceSource {
        store: scenario.store.clone(),
        row: engine.row.clone(),
        during_capture: false,
        publish_replacement: true,
        replaced: AtomicBool::new(false),
    }
    .replace();
    let source_version = library::observe_source(&engine.row.file.path)
        .unwrap()
        .source_version;
    scenario
        .store
        .as_ref()
        .put_file_meta_versioned(
            &engine.row.file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    capture_source: Some(support::fixture_source()),
                    is_default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Some(&source_version),
            Some(2_400_000),
        )
        .unwrap();
    before.cache_key = api::fingerprint_job::fingerprint_cache_key_with_profile(
        &source_version,
        180,
        Some(2_400_000),
        &api::fingerprint_job::profile_for_source(Some(&support::fixture_source())),
    );
    before.intro.words = vec![21, 22, 23];
    before.outro.as_mut().unwrap().words = vec![24, 25, 26];
    scenario
        .store
        .as_ref()
        .put_fingerprint_cache(&engine.row.file.file_id.to_string(), &before)
        .unwrap();
    scenario
        .store
        .file_coverage_by_id(engine.row.file.file_id)
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_with_the_same_sample_lengths_must_recompare() {
    let (scenario, engine) = scenario(true, false).await;
    let target = &scenario.ledgers[1];
    probe_once(&scenario, target.clone()).await;
    let first_job = scenario
        .store
        .latest_probe_job_for_ledger(&target.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let first_outcome: serde_json::Value = serde_json::from_str(
        scenario.store.probe_job_units(&first_job).unwrap()[0]
            .detection_outcome_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        scenario
            .store
            .as_ref()
            .get_file_marker(target.file.file_id, target.units[0])
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(10_000)
    );
    let current_reference = replace_with_equal_length_words(&scenario, &engine);
    probe_once(&scenario, current_reference).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while scenario
            .store
            .active_probe_unit_for_ledger(&target.file.file_id.to_string())
            .unwrap()
            .is_some()
        {
            advance_analysis(&scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    scenario.manager.shutdown_workers().await.unwrap();
    assert_eq!(
        scenario
            .store
            .as_ref()
            .get_file_marker(target.file.file_id, target.units[0])
            .unwrap()
            .unwrap()
            .intro_start_ms,
        Some(10_000),
        "one conflicting reference does not replace the supported marker"
    );
    let job = scenario
        .store
        .latest_probe_job_for_ledger(&target.file.file_id.to_string())
        .unwrap()
        .unwrap();
    assert_ne!(
        job, first_job,
        "a changed reference gives the earlier verified target its own successor"
    );
    let units = scenario.store.probe_job_units(&job).unwrap();
    let outcome: serde_json::Value =
        serde_json::from_str(units[0].detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_ne!(
        outcome["available_reference_key"],
        first_outcome["available_reference_key"]
    );
    assert_eq!(outcome["status"], "verified");
    let reference_job = scenario
        .store
        .latest_probe_job_for_ledger(&engine.row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let timings = [&job, &reference_job].map(|job| scenario.manager.timing_snapshot(job).unwrap());
    assert!(
        timings
            .iter()
            .filter_map(|t| t.raw_pairs_computed)
            .sum::<u64>()
            > 0,
        "equal-length changed words require actual fresh comparisons"
    );
    assert!(
        timings
            .iter()
            .filter_map(|t| t.raw_pairs_reused)
            .sum::<u64>()
            > 0,
        "unchanged ordered pairs keep their immutable results"
    );
}

impl marker::FingerprintCaptureEngine for ReplaceSource {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let words = marker::FingerprintEngine::extract_at(
            self,
            &request.path,
            (request.window.start_ms / 1000) as u32,
            (request.window.duration_ms() / 1000) as u32,
            &marker::WorkControl::default(),
        )
        .map_err(|message| marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Decode,
            message,
            metrics: marker::CaptureMetrics::default(),
        })?;
        Ok(support::fixture_capture_window(
            request.window.clone(),
            words,
        ))
    }
}
