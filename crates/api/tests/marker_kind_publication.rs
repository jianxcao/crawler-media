#[path = "probe_retry_support.rs"]
mod support;
use api::ScrapeStoreExt;
use api::probe_manager::clock::ProbeClock;
use api::probe_manager::{ProbeManager, ProbeUnit};
use marker::{CommonSegment, FingerprintEngine};
use std::sync::Arc;
use support::ProbeScenario;

struct KindEngine {
    intro_secs: f32,
    outro_secs: f32,
}
impl Default for KindEngine {
    fn default() -> Self {
        Self {
            intro_secs: 60.0,
            outro_secs: 3.0,
        }
    }
}
impl FingerprintEngine for KindEngine {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        unreachable!("capture engine supplies all samples")
    }
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> marker::PairFingerprintDecision {
        match self.find_common_segment(a, b, min, max) {
            Some(candidate) => marker::PairFingerprintDecision::Candidates(vec![candidate]),
            None => marker::PairFingerprintDecision::NoMatch,
        }
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        let duration = if first[0] < 100 {
            self.intro_secs
        } else {
            self.outro_secs
        };
        if duration == 0.0 {
            return None;
        }
        Some(CommonSegment {
            start1_sec: 30.0,
            end1_sec: 30.0 + duration,
            start2_sec: 30.0,
            end2_sec: 30.0 + duration,
            duration_sec: duration,
            score: 0.0,
        })
    }
}
impl marker::FingerprintCaptureEngine for KindEngine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Ok(support::fixture_capture_window(
            request.window.clone(),
            vec![if request.window.start_ms == 0 { 1 } else { 101 }, 2],
        ))
    }
}
async fn scenario(mode: &str) -> ProbeScenario {
    let mut s = ProbeScenario::cached_season();
    s.manager.shutdown_workers().await.unwrap();
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    s.store.save_scrape_config(&config).unwrap();
    for row in &s.ledgers {
        let id = row.file.file_id.to_string();
        let mut cache = s.store.get_fingerprint_cache(&id).unwrap().unwrap();
        cache.intro.words = vec![1, 2];
        cache.outro.as_mut().unwrap().words = vec![101, 2];
        s.store.put_fingerprint_cache(&id, &cache).unwrap();
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(
            s.store.clone(),
            Arc::new(KindEngine::default()),
            Arc::new(KindEngine::default()),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    s
}
fn restart(s: &mut ProbeScenario, engine: Arc<KindEngine>) {
    s.manager =
        Arc::new(ProbeManager::with_engines(s.store.clone(), engine.clone(), engine).unwrap());
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
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
async fn run(s: &ProbeScenario, refresh: bool) -> store::ProbeJob {
    if refresh {
        assert_eq!(
            s.manager
                .enqueue_marker_refresh(s.ledgers.iter().map(unit).collect()),
            s.ledgers.len()
        );
    } else {
        assert!(s.manager.enqueue(unit(&s.ledgers[0])));
    }
    wait(s, refresh).await
}
async fn wait(s: &ProbeScenario, refresh: bool) -> store::ProbeJob {
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = if refresh {
                s.store
                    .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                    .unwrap()
            } else {
                s.store
                    .latest_probe_job_for_scope(&format!("ledger:{}", s.ledgers[0].file.file_id))
                    .unwrap()
            };
            if let Some(job) = job
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
fn outcome(s: &ProbeScenario, job: &store::ProbeJob) -> serde_json::Value {
    let units = s.store.probe_job_units(&job.id).unwrap();
    serde_json::from_str(
        units
            .iter()
            .find(|u| u.ledger_id == s.ledgers[0].file.file_id.to_string())
            .unwrap()
            .detection_outcome_json
            .as_deref()
            .unwrap(),
    )
    .unwrap()
}
async fn partial(mode: &str, refresh: bool) {
    let mut s = scenario(mode).await;
    if mode == "adaptive" && !refresh {
        s.manager.shutdown_workers().await.unwrap();
        restart(
            &mut s,
            Arc::new(KindEngine {
                intro_secs: 60.0,
                outro_secs: 60.0,
            }),
        );
        assert_eq!(
            s.manager
                .enqueue_marker_refresh(s.ledgers[1..].iter().map(unit).collect()),
            7
        );
        assert_eq!(wait(&s, true).await.status, "succeeded");
        assert!(
            s.store
                .get_file_marker(s.ledgers[0].file.file_id, s.ledgers[0].units[0])
                .unwrap()
                .is_none()
        );
        restart(&mut s, Arc::new(KindEngine::default()));
    }
    let job = run(&s, refresh).await;
    let row = &s.ledgers[0];
    let marker = s
        .store
        .get_file_marker(row.file.file_id, row.units[0])
        .unwrap()
        .expect("independently verified Intro must publish");
    assert_eq!(
        (marker.intro_start_ms, marker.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    assert_eq!(marker.outro_start_ms, None);
    assert_eq!(job.status, "succeeded");
    let outcome = outcome(&s, &job);
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["intro"]["status"], "verified");
    assert_eq!(outcome["kinds"]["outro"]["status"], "uncertain");
    assert!(
        outcome["kinds"]["outro"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
    );
    let frames = s
        .store
        .current_chapter_frame_task(row.file.file_id)
        .unwrap()
        .expect("Intro publication must atomically create frame obligation");
    assert_eq!(frames.status.state, store::ChapterFrameState::Queued);
    let reopened = store::Store::open(s.tmp.path()).unwrap();
    assert_eq!(
        reopened
            .current_chapter_frame_task(row.file.file_id)
            .unwrap()
            .unwrap()
            .task,
        frames.task
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_fullwindow_publishes_verified_intro_independently() {
    partial("full_window", false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_fullwindow_publishes_verified_intro_independently() {
    partial("full_window", true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_publishes_verified_intro_independently() {
    partial("adaptive", false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_adaptive_publishes_verified_intro_independently() {
    partial("adaptive", true).await;
}

#[path = "marker_kind_publication/guards.rs"]
mod guards;
#[path = "marker_kind_publication/merge.rs"]
mod merge;
#[path = "marker_kind_publication/models.rs"]
mod models;

#[path = "marker_kind_publication/adaptive_absence.rs"]
mod adaptive_absence;
