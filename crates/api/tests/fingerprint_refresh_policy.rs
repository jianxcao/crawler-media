#[path = "fingerprint_refresh_policy/quantized.rs"]
mod quantized;
#[path = "probe_retry_support.rs"]
mod support;
use api::{
    fingerprint_job::{
        CapturePolicy, capture_profile_key, fingerprint_cache_key_with_profile, profile_for_source,
    },
    probe_manager::{ProbeManager, ProbeUnit},
};
use std::sync::Arc;

struct RefuseCapture;
impl marker::FingerprintCaptureEngine for RefuseCapture {
    fn capture_window(
        &self,
        _: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Io,
            message: "audio capture forbidden".into(),
            metrics: Default::default(),
        })
    }
}
impl marker::FingerprintEngine for RefuseCapture {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        Err("audio extraction forbidden".into())
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        Some(marker::CommonSegment {
            start1_sec: 30.,
            end1_sec: 90.,
            start2_sec: 30.,
            end2_sec: 90.,
            duration_sec: 60.,
            score: 0.,
        })
    }
}

fn facts() -> marker::CaptureSourceFacts {
    marker::CaptureSourceFacts {
        audio_stream_index: 1,
        format_start_time_us: Some(0),
        stream_start_pts: Some(0),
        stream_time_base: Some("1/16000".into()),
        codec_name: None,
        native_sample_rate: None,
        audio_channels: None,
        provenance: "fixture_header".into(),
    }
}
fn unit(row: &domain::FileCoverage) -> ProbeUnit {
    ProbeUnit {
        row: row.clone(),
        kind: domain::MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: true,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }
}
async fn terminal(s: &support::ProbeScenario) -> store::ProbeJob {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                .unwrap();
            if !job.is_active()
                && s.ledgers
                    .iter()
                    .all(|row| !s.manager.holds_dispatch_id(&row.file.file_id.to_string()))
            {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_reuses_valid_captures_but_recapture_failure_preserves_the_season() {
    let mut s = support::ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        s.store
            .put_file_meta_versioned(
                &id,
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    audio: vec![library::AudioTrack {
                        stream_index: Some(1),
                        capture_source: Some(facts()),
                        is_default: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Some(&row.file.version.source_version),
                Some(2_400_000),
            )
            .unwrap();
        let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
        let profile = profile_for_source(Some(&facts()));
        cache.cache_key = fingerprint_cache_key_with_profile(
            &row.file.version.source_version,
            180,
            Some(2_400_000),
            &profile,
        );
        for capture in std::iter::once(&mut cache.intro).chain(cache.outro.iter_mut()) {
            capture.metrics.time_mapping_version = 3;
            capture.metrics.pcm_timeline = marker::PcmTimeline::Contiguous;
            capture.metrics.contiguous_pcm_start_ms = Some(capture.window.start_ms);
            capture.metrics.audio_stream_index = Some(1);
            capture.metrics.source_facts = Some(facts());
        }
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
        assert!(!capture_profile_key(&profile).is_empty());
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(RefuseCapture),
            Arc::new(RefuseCapture),
        )
        .unwrap(),
    );
    s.manager.try_start_workers();
    assert!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ) > 0
    );
    assert_eq!(terminal(&s).await.status, "succeeded");
    let mut previous = Vec::new();
    for row in &s.ledgers {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            (Some(30_000), Some(90_000))
        );
        previous.push(marker);
    }
    assert!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::Recapture
        ) > 0
    );
    let failed = terminal(&s).await;
    assert_eq!(failed.status, "failed");
    assert!(failed.failed > 0);
    for (row, old) in s.ledgers.iter().zip(previous) {
        let current = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                current.intro_start_ms,
                current.intro_end_ms,
                current.outro_start_ms,
                current.outro_end_ms
            ),
            (
                old.intro_start_ms,
                old.intro_end_ms,
                old.outro_start_ms,
                old.outro_end_ms
            )
        );
    }
    s.manager.shutdown_workers().await.unwrap();
}

struct TailOnly;
impl marker::FingerprintEngine for TailOnly {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        Err("raw extraction forbidden".into())
    }
    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> Option<marker::CommonSegment> {
        RefuseCapture.find_common_segment(a, b, min, max)
    }
}
impl marker::FingerprintCaptureEngine for TailOnly {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if request.window.start_ms == 0 {
            return RefuseCapture.capture_window(request);
        }
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words: vec![1, 2, 3, 4],
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                pcm_timeline: marker::PcmTimeline::Contiguous,
                audio_stream_index: Some(1),
                source_facts: Some(facts()),
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                ..Default::default()
            },
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovered_recapture_job_reuses_its_successful_head_and_only_recovers_tail() {
    let s = support::ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    let job_id = "resume-recapture";
    let ids = seed_recovered_recapture(&s, job_id);
    let manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), Arc::new(TailOnly), Arc::new(TailOnly))
            .unwrap(),
    );
    manager.try_start_workers();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = s.store.get_probe_job(job_id).unwrap().unwrap();
            if !job.is_active()
                && s.ledgers
                    .iter()
                    .all(|row| !manager.holds_dispatch_id(&row.file.file_id.to_string()))
            {
                assert_eq!(job.status, "succeeded", "{job:?}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for id in &ids {
        let cache = s.store.get_fingerprint_cache(id).unwrap().unwrap();
        assert_eq!(cache.intro.metrics.captured_job_id.as_deref(), Some(job_id));
        assert_eq!(
            cache.outro.unwrap().metrics.captured_job_id.as_deref(),
            Some(job_id)
        );
    }
    manager.enqueue_marker_refresh_with_policy(
        s.ledgers.iter().map(unit).collect(),
        CapturePolicy::Recapture,
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                .unwrap();
            if job.id != job_id && !job.is_active() {
                assert_eq!(job.status, "failed");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for row in &s.ledgers {
        assert!(
            s.store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_some()
        );
    }
    manager.shutdown_workers().await.unwrap();
}

fn seed_recovered_recapture(s: &support::ProbeScenario, job_id: &str) -> Vec<String> {
    let ids = s
        .ledgers
        .iter()
        .map(|row| row.file.file_id.to_string())
        .collect::<Vec<_>>();
    let specs = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    assert!(
        s.store
            .create_probe_job(
                job_id,
                "marker_refresh",
                &s.media_id.to_string(),
                Some(1),
                &format!("marker-refresh:{}:1", s.media_id),
                &specs
            )
            .unwrap()
    );
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        s.store
            .put_file_meta_versioned(
                &id,
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    audio: vec![library::AudioTrack {
                        stream_index: Some(1),
                        capture_source: Some(facts()),
                        is_default: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Some(&row.file.version.source_version),
                Some(2_400_000),
            )
            .unwrap();
        let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
        cache.cache_key = fingerprint_cache_key_with_profile(
            &row.file.version.source_version,
            180,
            Some(2_400_000),
            &profile_for_source(Some(&facts())),
        );
        cache.outro = None;
        cache.intro.metrics.time_mapping_version = 3;
        cache.intro.metrics.pcm_timeline = marker::PcmTimeline::Contiguous;
        cache.intro.metrics.contiguous_pcm_start_ms = Some(0);
        cache.intro.metrics.audio_stream_index = Some(1);
        cache.intro.metrics.source_facts = Some(facts());
        cache.intro.metrics.captured_job_id = Some(job_id.into());
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
    }
    ids
}
