#[path = "probe_retry_support.rs"]
mod support;
use api::probe_manager::clock::ProbeClock;
use api::probe_manager::{ProbeManager, ProbeUnit};
use marker::{CommonSegment, FingerprintEngine};
use std::sync::Arc;
use support::ProbeScenario;

struct FixedCandidate;
impl FingerprintEngine for FixedCandidate {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("all samples are injected or cached")
    }
    fn find_common_segment(&self, _: &[u32], _: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        Some(CommonSegment {
            start1_sec: 30.0,
            end1_sec: 90.0,
            start2_sec: 30.0,
            end2_sec: 90.0,
            duration_sec: 60.0,
            score: 0.0,
        })
    }
}
impl marker::FingerprintCaptureEngine for FixedCandidate {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Ok(support::fixture_capture_window(
            request.window.clone(),
            vec![1, 2, 3, 4],
        ))
    }
}
async fn scenario() -> ProbeScenario {
    let mut scenario = ProbeScenario::cached_season();
    scenario.manager.shutdown_workers().await.unwrap();
    scenario.manager = Arc::new(
        ProbeManager::with_engines(
            scenario.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(FixedCandidate),
        )
        .unwrap(),
    );
    scenario.manager.set_clock(scenario.clock.clone());
    assert!(scenario.manager.try_start_workers());
    scenario
}
fn unit(row: &domain::FileCoverage, overwrite: bool) -> ProbeUnit {
    ProbeUnit {
        row: row.clone(),
        kind: domain::MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: overwrite,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }
}
async fn probe(scenario: &ProbeScenario) {
    let row = &scenario.ledgers[0];
    assert!(scenario.manager.enqueue(unit(row, false)));
    finish_probe(scenario).await;
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
async fn finish_probe(scenario: &ProbeScenario) {
    let row = &scenario.ledgers[0];
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if scenario
                .store
                .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
                .unwrap()
                .is_some_and(|job| !job.is_active())
                && !scenario.manager.is_queued(&row.file.file_id.to_string())
            {
                break;
            }
            advance_analysis(scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    scenario.manager.shutdown_workers().await.unwrap();
}
fn latest_outcome(scenario: &ProbeScenario) -> serde_json::Value {
    let id = scenario.ledgers[0].file.file_id.to_string();
    let job = scenario
        .store
        .latest_probe_job_for_ledger(&id)
        .unwrap()
        .unwrap();
    let units = scenario.store.probe_job_units(&job).unwrap();
    serde_json::from_str(
        units
            .iter()
            .find(|unit| unit.ledger_id == id)
            .unwrap()
            .detection_outcome_json
            .as_deref()
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fullwindow_publication_uses_captured_origin_and_actual_pcm_coverage() {
    struct ObservedWindow;
    impl marker::FingerprintCaptureEngine for ObservedWindow {
        fn capture_window(
            &self,
            request: &marker::CaptureRequest,
        ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
            // The capture seam reports its observed window. Publication must not
            // replace this origin with the requested fractional-duration offset.
            assert_eq!(
                request.window,
                marker::SampleWindow::new(1_620_750, 1_800_750)
            );
            Ok(support::fixture_capture_window(
                marker::SampleWindow::new(1_620_000, 1_800_000),
                vec![4, 5, 6],
            ))
        }
    }
    let mut scenario = scenario().await;
    scenario.manager.shutdown_workers().await.unwrap();
    for row in &scenario.ledgers {
        let key = row.file.file_id.to_string();
        let mut cache = scenario.store.get_fingerprint_cache(&key).unwrap().unwrap();
        cache.media_duration_ms = Some(1_800_750);
        let outro = cache.outro.as_mut().unwrap();
        *outro = support::fixture_capture_window(
            marker::SampleWindow::new(1_620_000, 1_800_000),
            outro.words.clone(),
        );
        scenario.store.put_fingerprint_cache(&key, &cache).unwrap();
        let tracks = scenario.store.get_file_meta(&key).unwrap().unwrap();
        scenario
            .store
            .put_file_meta_versioned(
                &key,
                &tracks,
                Some(&row.file.version.source_version),
                cache.media_duration_ms,
            )
            .unwrap();
    }
    scenario.manager = Arc::new(
        ProbeManager::with_engines(
            scenario.store.clone(),
            Arc::new(FixedCandidate),
            Arc::new(ObservedWindow),
        )
        .unwrap(),
    );
    assert!(scenario.manager.try_start_workers());
    probe(&scenario).await;
    let result = scenario
        .store
        .get_fixture_marker(scenario.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (result.intro_start_ms, result.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    assert_eq!(
        (result.outro_start_ms, result.outro_end_ms),
        (Some(1_650_000), Some(1_710_000))
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_word_arrays_do_not_publish_incomplete_pcm() {
    let scenario = scenario().await;
    for row in &scenario.ledgers {
        let key = row.file.file_id.to_string();
        let mut cache = scenario.store.get_fingerprint_cache(&key).unwrap().unwrap();
        cache.intro.words = (1..10_000).collect();
        cache.intro.pcm_duration_ms = Some(90_000);
        scenario.store.put_fingerprint_cache(&key, &cache).unwrap();
    }
    probe(&scenario).await;
    let result = scenario
        .store
        .get_fixture_marker(scenario.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        result.intro_start_ms, None,
        "incomplete Intro PCM cannot publish Intro"
    );
    assert_eq!(result.outro_start_ms, Some(2_250_000));
    let outcome = latest_outcome(&scenario);
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
    assert_eq!(outcome["kinds"]["outro"]["status"], "verified");
}
async fn assert_uncertain_refresh_preserves_previous(mode: &str) {
    use api::ScrapeStoreExt;
    let scenario = scenario().await;
    let mut config = scenario.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    scenario.store.save_scrape_config(&config).unwrap();
    let old = store::StoredFileMarker {
        intro_start_ms: Some(11_000),
        intro_end_ms: Some(80_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fingerprint".into(),
        locked: false,
        updated_at: 1,
        ..store::test_support::fixture_marker(
            &scenario.store,
            scenario.media_id,
            domain::UnitKey::new(1, 1),
        )
    };
    scenario.store.put_fixture_marker(&old).unwrap();
    for row in &scenario.ledgers {
        scenario
            .store
            .put_cached_chapters(
                &row.file.file_id.to_string(),
                &[marker::ChapterMarker {
                    start_ms: 20_000,
                    end_ms: 95_000,
                    title: Some("Source opening".into()),
                    marker_type: Some(marker::MarkerType::IntroStart),
                    synthetic: false,
                }],
            )
            .unwrap();
    }
    assert_eq!(
        scenario
            .manager
            .enqueue_marker_refresh(scenario.ledgers.iter().map(|row| unit(row, true)).collect()),
        scenario.ledgers.len()
    );
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = scenario
                .store
                .latest_marker_refresh_for_season(&scenario.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
            {
                break job;
            }
            advance_analysis(&scenario);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    scenario.manager.shutdown_workers().await.unwrap();
    assert_eq!(job.status, "succeeded");
    let outcome = latest_outcome(&scenario);
    assert_eq!(outcome["status"], "uncertain");
    assert!(
        outcome["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
    let after = scenario
        .store
        .get_fixture_marker(scenario.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(after.intro_start_ms, old.intro_start_ms);
    assert_eq!(after.intro_end_ms, old.intro_end_ms);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_fullwindow_refresh_preserves_previous_marker_and_reason() {
    assert_uncertain_refresh_preserves_previous("full_window").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_adaptive_refresh_preserves_previous_marker_and_reason() {
    assert_uncertain_refresh_preserves_previous("adaptive").await;
}

async fn assert_ordinary_uncertainty_waits_for_explicit_refresh(mode: &str) {
    use api::ScrapeStoreExt;
    let scenario = scenario().await;
    let mut config = scenario.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    scenario.store.save_scrape_config(&config).unwrap();
    let old = store::StoredFileMarker {
        intro_start_ms: Some(11_000),
        intro_end_ms: Some(80_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fingerprint".into(),
        locked: false,
        updated_at: 1,
        ..store::test_support::fixture_marker(
            &scenario.store,
            scenario.media_id,
            domain::UnitKey::new(1, 1),
        )
    };
    scenario.store.put_fixture_marker(&old).unwrap();
    for row in &scenario.ledgers {
        scenario
            .store
            .put_cached_chapters(
                &row.file.file_id.to_string(),
                &[marker::ChapterMarker {
                    start_ms: 20_000,
                    end_ms: 95_000,
                    title: Some("Source opening".into()),
                    marker_type: Some(marker::MarkerType::IntroStart),
                    synthetic: false,
                }],
            )
            .unwrap();
    }
    probe(&scenario).await;
    let row = &scenario.ledgers[0];
    let job = scenario
        .store
        .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
        .unwrap()
        .unwrap();
    assert!(!job.is_active(), "sampling job must finish: {job:?}");
    let units = scenario.store.probe_job_units(&job.id).unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        units[0]
            .detection_outcome_json
            .as_deref()
            .expect("uncertainty must be durable"),
    )
    .unwrap();
    assert_eq!(outcome["status"], "uncertain");
    assert!(
        outcome["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
    let after = scenario
        .store
        .get_fixture_marker(scenario.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms)
    );
    let result = scenario
        .manager
        .request_probe(
            row,
            api::probe_manager::policy::ProbeRequestOrigin::BackgroundRetry,
        )
        .unwrap();
    assert!(
        matches!(
            result,
            api::probe_manager::policy::ProbeRequestResult::Complete
        ),
        "same evidence must not cause automatic re-sampling: {result:?}"
    );
    assert_eq!(scenario.manager.dispatch_due(i64::MAX / 2, 8).unwrap(), 0);
    let refresh = ProbeManager::with_engines(
        scenario.store.clone(),
        Arc::new(FixedCandidate),
        Arc::new(FixedCandidate),
    )
    .unwrap();
    assert_eq!(
        refresh.enqueue_marker_refresh(vec![unit(row, true)]),
        1,
        "explicit refresh can re-analyze cached evidence"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_fullwindow_uncertainty_waits_for_explicit_refresh() {
    assert_ordinary_uncertainty_waits_for_explicit_refresh("full_window").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_uncertainty_waits_for_explicit_refresh() {
    assert_ordinary_uncertainty_waits_for_explicit_refresh("adaptive").await;
}

#[path = "marker_boundary_publication/current_duration.rs"]
mod current_duration;
#[path = "marker_boundary_publication/final_lifecycle.rs"]
mod final_lifecycle;
#[path = "marker_boundary_publication/job_scope.rs"]
mod job_scope;
#[path = "marker_boundary_publication/pair_cache.rs"]
mod pair_cache;
#[path = "marker_boundary_publication/reader_pressure.rs"]
mod reader_pressure;
#[path = "marker_boundary_publication/review_regressions.rs"]
mod review_regressions;
