use super::support::{self, ProbeScenario};
use api::{
    ScrapeStoreExt,
    probe_manager::{ProbeManager, ProbeUnit, clock::ProbeClock},
};
use marker::{CommonSegment, FingerprintEngine};
use std::sync::{Arc, Mutex};
pub type CaptureHook = Arc<dyn Fn(&marker::CaptureRequest) + Send + Sync>;
pub struct LateEngine {
    pub start: i64,
    pub mixed: std::sync::atomic::AtomicBool,
    pub end: i64,
    pub fault: Mutex<Option<&'static str>>,
    pub requests: Mutex<Vec<(String, marker::SampleWindow)>>,
    pub on_capture: Mutex<Option<CaptureHook>>,
}
impl FingerprintEngine for LateEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!()
    }
    fn find_common_segment(&self, a: &[u32], b: &[u32], _: f32, _: f32) -> Option<CommonSegment> {
        let (start, end) = (self.start, self.end);
        let (start_b, end_b) = if self.mixed.load(std::sync::atomic::Ordering::Relaxed) && b[2] != 1
        {
            (30_000, 110_000)
        } else {
            (start, end)
        };
        let (start, end) = if self.mixed.load(std::sync::atomic::Ordering::Relaxed) && a[2] != 1 {
            (30_000, 110_000)
        } else {
            (start, end)
        };
        if a[0] as i64 > start
            || b[0] as i64 > start_b
            || (a[1] as i64) < end
            || (b[1] as i64) < end_b
            || start < 0
        {
            return None;
        }
        Some(CommonSegment {
            start1_sec: (start - a[0] as i64) as f32 / 1000.,
            end1_sec: (end - a[0] as i64) as f32 / 1000.,
            start2_sec: (start_b - b[0] as i64) as f32 / 1000.,
            end2_sec: (end_b - b[0] as i64) as f32 / 1000.,
            duration_sec: (end - start) as f32 / 1000.,
            score: 0.,
        })
    }
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        match self.find_common_segment(a, b, min, max) {
            Some(c) => marker::PairFingerprintDecision::Candidates(vec![c]),
            None => marker::PairFingerprintDecision::NoMatch,
        }
    }
}
impl marker::FingerprintCaptureEngine for LateEngine {
    fn capture_window(
        &self,
        r: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.requests
            .lock()
            .unwrap()
            .push((r.path.to_string_lossy().into(), r.window.clone()));
        let hook = self.on_capture.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(r);
        }
        let episode = r
            .path
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .rsplit('E')
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let mut capture = support::fixture_capture_window(
            r.window.clone(),
            vec![
                r.window.start_ms as u32,
                r.window.end_ms as u32,
                episode,
                19,
            ],
        );
        match *self.fault.lock().unwrap() {
            Some("unknown") => capture.metrics.pcm_timeline = marker::PcmTimeline::Unknown,
            Some("invalid") => {
                capture.metrics.pcm_timeline = marker::PcmTimeline::Invalid("gap".into())
            }
            Some("leading") => {
                capture.metrics.contiguous_pcm_start_ms = Some(r.window.start_ms + 500)
            }
            Some("source") => {
                capture
                    .metrics
                    .source_facts
                    .as_mut()
                    .unwrap()
                    .audio_stream_index = 9
            }
            Some("incomplete") => capture.pcm_duration_ms = Some(r.window.duration_ms() - 5_000),
            _ => {}
        }
        Ok(capture)
    }
}
pub async fn scenario(
    count: usize,
    start: i64,
    end: i64,
    duration: Option<i64>,
) -> (ProbeScenario, Arc<LateEngine>) {
    let mut s = ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    s.ledgers.truncate(count);
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some("adaptive".into());
    s.store.save_scrape_config(&config).unwrap();
    if let Some(duration) = duration {
        for row in &s.ledgers {
            let tracks = library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_secs: (duration > 0).then_some(duration as f64 / 1000.),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    is_default: true,
                    capture_source: Some(support::fixture_source()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            s.store
                .put_file_meta_versioned(
                    &row.file.file_id.to_string(),
                    &tracks,
                    Some(&row.file.version.source_version),
                    (duration > 0).then_some(duration),
                )
                .unwrap();
        }
    }
    let engine = Arc::new(LateEngine {
        start,
        end,
        mixed: std::sync::atomic::AtomicBool::new(false),
        fault: Mutex::new(None),
        requests: Mutex::new(vec![]),
        on_capture: Mutex::new(None),
    });
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), engine.clone())
            .unwrap()
            .with_tracks_probe(Arc::new(FixtureTracks {
                duration: duration.unwrap_or(2_400_000),
            })),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    (s, engine)
}
pub fn unit(row: &domain::FileCoverage) -> ProbeUnit {
    ProbeUnit {
        row: row.clone(),
        kind: domain::MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }
}
pub async fn run(s: &ProbeScenario) -> store::ProbeJob {
    assert_eq!(
        s.manager
            .enqueue_marker_refresh(s.ledgers.iter().map(unit).collect()),
        s.ledgers.len()
    );
    wait(s).await
}
pub async fn wait(s: &ProbeScenario) -> store::ProbeJob {
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
            {
                break job;
            }
            if !s
                .store
                .pending_probe_analysis_keys(None, 1)
                .unwrap()
                .is_empty()
            {
                s.clock.set(s.clock.now_ms() + 2_000);
                s.manager.wake_dispatch();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    s.manager.shutdown_workers().await.unwrap();
    job
}
pub fn assert_windows(s: &ProbeScenario, engine: &LateEngine, expected: &[(i64, i64)]) {
    for row in &s.ledgers {
        let windows = engine
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == &row.file.path.to_string_lossy())
            .map(|(_, w)| (w.start_ms, w.end_ms))
            .collect::<Vec<_>>();
        let intros = windows
            .iter()
            .copied()
            .filter(|(start, _)| *start < 600_000)
            .collect::<Vec<_>>();
        assert_eq!(intros, expected);
        if row.file.version.source_version.is_empty() {
            panic!("source identity required");
        }
        if windows.iter().any(|(start, _)| *start >= 600_000) {
            assert_eq!(
                windows
                    .iter()
                    .filter(|(start, _)| *start >= 600_000)
                    .copied()
                    .collect::<Vec<_>>(),
                vec![(2_220_000, 2_400_000)]
            );
        }
    }
}

struct FixtureTracks {
    duration: i64,
}
impl api::probe_manager::TracksProbe for FixtureTracks {
    fn probe(
        &self,
        _: &std::path::Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    is_default: true,
                    capture_source: Some(support::fixture_source()),
                    ..Default::default()
                }],
                ..Default::default()
            },
            (self.duration > 0).then_some(self.duration),
        ))
    }
}
