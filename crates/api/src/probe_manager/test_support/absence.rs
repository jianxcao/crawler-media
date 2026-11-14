//! Complete, independent zero-candidate evidence for refresh lifecycle tests.
use std::sync::Arc;

use crate::ScrapeStoreExt;
use crate::probe_manager::ProbeManager;

fn source_facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 0,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture_header".into(),
    }
}

fn tracks() -> library::Tracks {
    library::Tracks {
        video: Some(Default::default()),
        audio: vec![library::AudioTrack {
            stream_index: Some(0),
            is_default: true,
            capture_source: Some(source_facts()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

struct NoCandidates;
impl marker::FingerprintEngine for NoCandidates {
    fn compare_fingerprints(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        let candidates = self.find_common_segments(first, second, min, max);
        if candidates.is_empty() {
            marker::PairFingerprintDecision::NoMatch
        } else {
            marker::PairFingerprintDecision::Candidates(candidates)
        }
    }

    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("lifecycle fixture supplies all captured evidence")
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
impl marker::FingerprintCaptureEngine for NoCandidates {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        unreachable!("lifecycle fixture finishes queued captures explicitly")
    }
}

pub(in crate::probe_manager) fn confirmed_absence_manager(
    store: Arc<store::Store>,
    rows: &[domain::FileCoverage],
) -> ProbeManager {
    let mut setting = store.get_scrape_config().unwrap().setting;
    setting.fingerprint_duration_secs = Some(300);
    store.save_scrape_config(&setting).unwrap();
    for row in rows {
        // References outside this job also need current same-source metadata;
        // a cached tail alone cannot prove the current media endpoint.
        store
            .put_file_meta_versioned(
                &row.file.file_id.to_string(),
                &tracks(),
                Some(&row.file.version.source_version),
                Some(1_100_000),
            )
            .unwrap();
        let capture = |start| marker::CapturedFingerprint {
            window: marker::SampleWindow::new(start, start + 300_000),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(300_000),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                source_facts: Some(source_facts()),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(start),
                ..Default::default()
            },
        };
        store
            .put_fingerprint_cache(
                &row.file.file_id.to_string(),
                &store::FingerprintCacheEntry {
                    cache_key: crate::fingerprint_job::fingerprint_cache_key_with_profile(
                        &row.file.version.source_version,
                        300,
                        Some(1_100_000),
                        &crate::fingerprint_job::profile_for_source(Some(&source_facts())),
                    ),
                    algorithm_version: marker::FINGERPRINT_ALGORITHM_VERSION,
                    sample_duration_secs: 300,
                    media_duration_ms: Some(1_100_000),
                    intro: capture(0),
                    outro: Some(capture(800_000)),
                },
            )
            .unwrap();
    }
    ProbeManager::with_engines(store, Arc::new(NoCandidates), Arc::new(NoCandidates)).unwrap()
}

pub(in crate::probe_manager) fn handoff_cached(
    manager: &ProbeManager,
    unit: &crate::probe_manager::ProbeUnit,
) {
    let store = &manager.store;
    let job = unit.job_id.as_ref().unwrap();
    let file = unit.row.file.file_id.to_string();
    let cache = store.get_fingerprint_cache(&file).unwrap().unwrap();
    store
        .put_file_meta_versioned(
            &file,
            &tracks(),
            Some(&unit.row.file.version.source_version),
            cache.media_duration_ms,
        )
        .unwrap();
    store.start_probe_unit(job, &file).unwrap();
    assert!(store.advance_probe_unit_to_fingerprint(job, &file).unwrap());
    assert!(store.start_probe_unit(job, &file).unwrap());
    let policy = marker::SamplingPolicy {
        full_window_duration_secs: cache.sample_duration_secs,
        ..Default::default()
    };
    store
        .pin_probe_job_sampling_plan(
            job,
            &serde_json::json!({"searches":[],"selected_samples":[],"policy":policy,"sampling_mode":"full_window"})
                .to_string(),
        )
        .unwrap();
    crate::probe_manager::stages::record_fingerprint_stages(
        manager,
        &file,
        job,
        &cache.cache_key,
        None,
        0,
    )
    .unwrap();
    crate::probe_manager::analysis::handoff(manager, unit).unwrap();
}

pub(in crate::probe_manager) fn drain_analysis(manager: Arc<ProbeManager>, job: &str) {
    let manager = Arc::new(
        ProbeManager::with_comparison_session(
            manager.store.clone(),
            manager.fingerprint_engine.clone(),
            manager.capture_engine.clone(),
        )
        .unwrap(),
    );
    let clock = Arc::new(crate::probe_manager::clock::FakeClock::new(0));
    manager.set_clock(clock.clone());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        manager.try_start_workers();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut now = 0;
            while manager
                .store
                .get_probe_job(job)
                .unwrap()
                .unwrap()
                .is_active()
            {
                now += 2_000;
                clock.set(now);
                manager.wake_dispatch();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!("{error}: {:?}", manager.store.recover_probe_jobs().unwrap())
        });
        manager.shutdown_workers().await.unwrap();
    });
}
