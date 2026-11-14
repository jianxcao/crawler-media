#[path = "fingerprint_source_binding/fixtures.rs"]
mod fixtures;
#[path = "probe_retry_support.rs"]
mod retry;
use api::fingerprint_job::{
    capture_profile_key, fingerprint_cache_key_with_profile, profile_for_source,
};
use api::probe_manager::clock::ProbeClock;
use fixtures::*;
use std::sync::Arc;

struct RefuseAudio;
impl marker::FingerprintCaptureEngine for RefuseAudio {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "audio forbidden".into(),
            metrics: Default::default(),
        })
    }
}
impl marker::FingerprintEngine for RefuseAudio {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("only stored captures are inputs")
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> marker::PairFingerprintDecision {
        marker::PairFingerprintDecision::NoMatch
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

#[tokio::test]
async fn full_reuse_binds_head_and_tail_to_current_metadata() {
    let s = scenario().await;
    let row = &s.ledgers[0];
    let old = s
        .store
        .get_fingerprint_cache(&row.file.file_id.to_string())
        .unwrap()
        .unwrap();
    let control = marker::WorkControl::default();
    let run = |cached, current| {
        api::fingerprint_job::capture_or_reuse_fingerprints_from_source(
            &row.file.path,
            &row.file.version.source_version,
            180,
            Some(2_400_000),
            Some(cached),
            true,
            Arc::new(RefuseAudio),
            &control,
            Some(current),
        )
    };
    let valid = run(old.clone(), source(48_000)).await.unwrap();
    assert!(valid.intro_cache_hit && valid.outro_cache_hit);
    metadata(&s, 44_100);
    assert_eq!(
        s.store
            .get_fingerprint_cache(&row.file.file_id.to_string())
            .unwrap()
            .unwrap(),
        old
    );
    let current = api::fingerprint_job::capture_source(
        &s.store
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .unwrap(),
        Some(1),
    )
    .unwrap();
    // The mapping profile intentionally aliases these supported rates; current facts must bind them.
    assert_eq!(
        profile_key(),
        capture_profile_key(&profile_for_source(Some(&current)))
    );
    let head_refused = run(old.clone(), current.clone()).await.is_err();
    let mut tail_only = old;
    tail_only.intro.metrics.source_facts = Some(current.clone());
    tail_only
        .intro
        .metrics
        .quantized_mapping
        .as_mut()
        .unwrap()
        .native_sample_rate = 44_100;
    let result = run(tail_only, current).await.unwrap();
    assert!(result.intro_cache_hit);
    assert!(
        head_refused && !result.outro_cache_hit && result.outro_error.is_some(),
        "self-consistent old48 head/tail must be recaptured; head_refused={head_refused}, tail_hit={}",
        result.outro_cache_hit
    );
}

#[tokio::test]
async fn adaptive_reuse_rejects_self_consistent_old_rate_sample() {
    let s = scenario().await;
    let row = &s.ledgers[0];
    let request = api::fingerprint_job::EpisodeCaptureRequest {
        job_id: String::new(),
        row: row.clone(),
        source_version: row.file.version.source_version.clone(),
        media_duration_ms: Some(2_400_000),
        audio_stream_index: Some(1),
        capture_profile_key: profile_key(),
        policy: Default::default(),
        capture_policy: api::fingerprint_job::CapturePolicy::ReuseValid,
        templates: Default::default(),
        cost_summary: Default::default(),
    };
    let window = marker::SampleWindow::new(0, 180_000);
    let valid = api::fingerprint_job::adaptive::find_reusable_sample(
        &s.store,
        &request,
        marker::SegmentKind::Intro,
        &window,
    )
    .unwrap();
    metadata(&s, 44_100);
    let stored = s
        .store
        .fingerprint_sample_by_id(&valid.sample_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.metrics_json,
        serde_json::to_string(&valid.capture.metrics).unwrap()
    );
    assert!(
        api::fingerprint_job::adaptive::find_reusable_sample(
            &s.store,
            &request,
            marker::SegmentKind::Intro,
            &window
        )
        .is_none()
    );
}

async fn analysis_selection(mode: &str, refresh: bool) {
    for corrected in [false, true] {
        let mut s = scenario().await;
        if corrected {
            metadata(&s, 44_100);
        }
        let old_markers = s
            .ledgers
            .iter()
            .map(|row| {
                let old = store::StoredFileMarker {
                    file_id: row.file.file_id,
                    version: row.file.version.clone(),
                    unit: row.units[0],
                    intro_start_ms: Some(120_000),
                    intro_end_ms: Some(180_000),
                    outro_start_ms: Some(2_330_000),
                    outro_end_ms: Some(2_390_000),
                    source: "fingerprint".into(),
                    locked: false,
                    updated_at: 1,
                };
                s.store.put_fixture_marker(&old).unwrap();
                s.store
                    .get_file_marker(row.file.file_id, row.units[0])
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let job = analysis_job(&s, mode, refresh);
        s.manager = Arc::new(
            api::probe_manager::ProbeManager::with_engines(
                s.store.clone(),
                Arc::new(RefuseAudio),
                Arc::new(RefuseAudio),
            )
            .unwrap(),
        );
        s.manager.set_clock(s.clock.clone());
        assert!(s.manager.try_start_workers());
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let job_row = s.store.get_probe_job(&job).unwrap().unwrap();
                if !job_row.is_active() {
                    break job_row;
                }
                s.set_time_ms(s.clock.now_ms() + 2_000);
                s.manager.wake_dispatch();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "{mode} corrected={corrected}: {error}: units={:?}",
                s.store.probe_job_units(&job).unwrap()
            )
        });
        s.manager.shutdown_workers().await.unwrap();
        assert_eq!(
            terminal.status, "succeeded",
            "ranged absence is a completed analysis with conservative typed uncertainty"
        );
        for (row, old) in s.ledgers.iter().zip(old_markers) {
            assert_eq!(
                s.store
                    .get_file_marker(row.file.file_id, row.units[0])
                    .unwrap()
                    .unwrap(),
                old
            );
        }
        for unit in s.store.probe_job_units(&job).unwrap() {
            let outcome: serde_json::Value =
                serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
            let references = outcome["reference_evidence"].as_array().unwrap();
            if corrected {
                assert!(
                    references.is_empty(),
                    "{mode} selected stale48 captures with current44100 metadata: {outcome}"
                );
            } else {
                assert!(
                    !references.is_empty(),
                    "same-rate valid captures remain Analysis inputs"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_analysis_excludes_self_consistent_old_rate_captures() {
    analysis_selection("full_window", true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adaptive_analysis_excludes_self_consistent_old_rate_samples() {
    analysis_selection("adaptive", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_full_analysis_retires_rejected_current_rate_input() {
    analysis_selection("full_window", false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_analysis_retires_rejected_current_rate_input() {
    analysis_selection("adaptive", false).await;
}
