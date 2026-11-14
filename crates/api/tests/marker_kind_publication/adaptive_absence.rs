use super::*;
use marker::{FingerprintCaptureEngine, PairFingerprintDecision, SampleWindow};
use std::sync::Mutex;

struct TailEngine {
    missing_target: bool,
    truncated: bool,
    requests: Mutex<Vec<SampleWindow>>,
}
impl FingerprintEngine for TailEngine {
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
        let tail = a[0] >= 600_000;
        if tail && self.missing_target && (a[2] == 1 || b[2] == 1) {
            return None;
        }
        let start = if tail { 2_250_000 } else { 30_000 };
        let end = start + 60_000;
        if a[0] > start || b[0] > start || a[1] < end || b[1] < end {
            return None;
        }
        Some(CommonSegment {
            start1_sec: (start - a[0]) as f32 / 1000.,
            end1_sec: (end - a[0]) as f32 / 1000.,
            start2_sec: (start - b[0]) as f32 / 1000.,
            end2_sec: (end - b[0]) as f32 / 1000.,
            duration_sec: 60.,
            score: 0.,
        })
    }
    fn compare_fingerprints(
        &self,
        a: &[u32],
        b: &[u32],
        min: f32,
        max: f32,
    ) -> PairFingerprintDecision {
        match self.find_common_segment(a, b, min, max) {
            Some(candidate) => PairFingerprintDecision::Candidates(vec![candidate]),
            None => PairFingerprintDecision::NoMatch,
        }
    }
}
impl FingerprintCaptureEngine for TailEngine {
    fn capture_window(
        &self,
        r: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.requests.lock().unwrap().push(r.window.clone());
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
        let mut captured = support::fixture_capture_window(
            r.window.clone(),
            vec![
                r.window.start_ms as u32,
                r.window.end_ms as u32,
                episode,
                19,
            ],
        );
        if self.truncated && r.window.start_ms >= 600_000 {
            captured.pcm_duration_ms = Some(r.window.duration_ms() - 5_000);
        }
        Ok(captured)
    }
}
fn engine(missing_target: bool, truncated: bool) -> Arc<TailEngine> {
    Arc::new(TailEngine {
        missing_target,
        truncated,
        requests: Mutex::new(vec![]),
    })
}
fn start(s: &mut ProbeScenario, engine: Arc<TailEngine>) {
    s.manager =
        Arc::new(ProbeManager::with_engines(s.store.clone(), engine.clone(), engine).unwrap());
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
}
async fn seeded() -> ProbeScenario {
    let mut s = scenario("adaptive").await;
    s.manager.shutdown_workers().await.unwrap();
    start(&mut s, engine(false, false));
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers[1..].iter().map(unit).collect(),
            api::fingerprint_job::CapturePolicy::Recapture,
        ),
        7
    );
    assert_eq!(wait(&s, true).await.status, "succeeded");
    s
}
fn full_plan(s: &ProbeScenario, refresh: bool) -> String {
    let id = s.ledgers[0].file.file_id.to_string();
    let job = "recovered-full-tail";
    assert!(
        s.store
            .create_probe_job(
                job,
                if refresh {
                    "marker_refresh"
                } else {
                    "fingerprint_probe"
                },
                &s.media_id.to_string(),
                Some(1),
                &if refresh {
                    format!("marker_refresh:{}:1", s.media_id)
                } else {
                    format!("ledger:{id}")
                },
                &[store::ProbeJobUnitSpec {
                    ledger_id: &id,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: true,
                    reuse_media_info_cache: true
                }],
            )
            .unwrap()
    );
    let plan = serde_json::json!({"searches":[
        {"kind":"intro","windows":[],"pending":{"start_ms":0,"end_ms":180_000},"terminal_reason":null},
        {"kind":"outro","windows":[],"pending":{"start_ms":2_220_000,"end_ms":2_400_000},"terminal_reason":null}
    ],"selected_samples":[],"policy":marker::SamplingPolicy { max_windows_per_kind:3, ..Default::default() },"sampling_mode":"adaptive"});
    assert!(
        s.store
            .put_probe_sampling_plan(job, &id, &plan.to_string())
            .unwrap()
    );
    job.into()
}
async fn absence(refresh: bool, narrow: bool, truncated: bool, covered: bool) {
    let mut s = seeded().await;
    let mut old = merge::old_marker(&s, false);
    if !covered {
        old.outro_start_ms = Some(2_100_000);
        old.outro_end_ms = Some(2_150_000);
        s.store.put_fixture_marker(&old).unwrap();
    }
    let engine = engine(true, truncated);
    if !narrow {
        full_plan(&s, refresh);
    }
    start(&mut s, engine.clone());
    if narrow {
        let mut request = unit(&s.ledgers[0]);
        request.reuse_fingerprint_cache = false;
        if refresh {
            assert_eq!(s.manager.enqueue_marker_refresh(vec![request]), 1);
        } else {
            assert!(s.manager.enqueue(request));
        }
    }
    let job = wait(&s, refresh).await;
    assert_eq!(job.status, "succeeded");
    let after = merge::marker(&s);
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    let clears = covered && !truncated;
    assert_eq!(
        (after.outro_start_ms, after.outro_end_ms),
        if clears {
            (None, None)
        } else {
            (old.outro_start_ms, old.outro_end_ms)
        }
    );
    let result = outcome(&s, &job);
    assert_eq!(
        result["kinds"]["outro"]["status"],
        if clears { "no_match" } else { "uncertain" }
    );
    assert_eq!(result["kinds"]["outro"]["retained_previous"], !clears);
    assert_eq!(result["kinds"]["intro"]["retained_previous"], false);
    if !truncated {
        assert_eq!(result["kinds"]["outro"]["decision"], "no_match");
    }
    let tails = engine
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|w| w.start_ms >= 600_000)
        .map(|w| (w.start_ms, w.end_ms))
        .collect::<Vec<_>>();
    assert_eq!(
        tails,
        if narrow && truncated {
            vec![(2_240_000, 2_320_000)]
        } else if narrow {
            vec![(2_240_000, 2_320_000), (2_220_000, 2_400_000)]
        } else {
            vec![(2_220_000, 2_400_000)]
        }
    );
    let units = s.store.probe_job_units(&job.id).unwrap();
    let plan: serde_json::Value =
        serde_json::from_str(units[0].sampling_plan_json.as_deref().unwrap()).unwrap();
    let selected = plan["selected_samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            s.store
                .fingerprint_sample_by_id(id.as_str().unwrap())
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(selected.iter().any(|sample| sample.kind == "outro"
        && sample.window_start_ms
            == if narrow && truncated {
                2_240_000
            } else {
                2_220_000
            }
        && sample.window_end_ms
            == if narrow && truncated {
                2_320_000
            } else {
                2_400_000
            }
        && sample.captured_job_id == job.id));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_initial_full_tail_preserves_strict_absence() {
    absence(false, false, false, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_adaptive_initial_full_tail_preserves_strict_absence() {
    absence(true, false, false, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_adaptive_narrow_then_full_tail_preserves_strict_absence() {
    absence(false, true, false, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_adaptive_narrow_then_full_tail_preserves_strict_absence() {
    absence(true, true, false, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adaptive_full_tail_absence_retains_truncated_and_outside_markers() {
    for refresh in [false, true] {
        for narrow in [false, true] {
            absence(refresh, narrow, true, true).await;
            absence(refresh, narrow, false, false).await;
        }
    }
}
