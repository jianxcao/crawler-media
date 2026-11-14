use super::*;
use marker::{CommonSegment, FingerprintCaptureEngine, FingerprintEngine};
use std::sync::Arc;
struct PerWindow;
impl FingerprintEngine for PerWindow {
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
        if a[0] != b[0] {
            return None;
        }
        let start = match a[0] {
            0 => 150_000,
            10_000 => 149_200,
            20_000 => 150_800,
            _ => return None,
        };
        let relative = (start - a[0]) as f32 / 1000.;
        Some(CommonSegment {
            start1_sec: relative,
            end1_sec: relative + 20.,
            start2_sec: relative,
            end2_sec: relative + 20.,
            duration_sec: 20.,
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
impl FingerprintCaptureEngine for PerWindow {
    fn capture_window(
        &self,
        r: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        Ok(support::fixture_capture_window(
            r.window.clone(),
            vec![r.window.start_ms as u32, r.window.end_ms as u32, 7, 19],
        ))
    }
}
fn prepare(s: &support::ProbeScenario) -> String {
    let ids = s
        .ledgers
        .iter()
        .map(|r| r.file.file_id.to_string())
        .collect::<Vec<_>>();
    let specs = ids
        .iter()
        .map(|id| store::ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: true,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect::<Vec<_>>();
    let job = "middle-first-conflict";
    assert!(
        s.store
            .create_probe_job(
                job,
                "marker_refresh",
                &s.media_id.to_string(),
                Some(1),
                &format!("marker_refresh:{}:1", s.media_id),
                &specs
            )
            .unwrap()
    );
    for (index, row) in s.ledgers.iter().enumerate() {
        let selected = (0..3)
            .map(|slot| {
                let id = format!("window-{index}-{slot}");
                evidence::put_sample(
                    s,
                    row,
                    &id,
                    marker::SampleWindow::new(slot * 10_000, 180_000 + slot * 10_000),
                    job,
                );
                id
            })
            .collect::<Vec<_>>();
        let windows = (0..3)
            .map(|slot| marker::SampleWindow::new(slot * 10_000, 180_000 + slot * 10_000))
            .collect::<Vec<_>>();
        let plan = serde_json::json!({"searches":[
            {"kind":"intro","windows":windows,"pending":{"start_ms":0,"end_ms":180_000},"terminal_reason":null},
            {"kind":"outro","windows":[],"pending":{"start_ms":2_220_000,"end_ms":2_400_000},"terminal_reason":null}
        ],"selected_samples":selected,"policy":marker::SamplingPolicy {max_windows_per_kind:3,..Default::default()},"sampling_mode":"adaptive"});
        assert!(
            s.store
                .put_probe_sampling_plan(job, &row.file.file_id.to_string(), &plan.to_string())
                .unwrap()
        );
    }
    job.into()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn middle_first_verified_windows_must_still_reject_the_outer_pair_conflict() {
    let (mut s, _) = scenario(3, -1, 0, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let job = prepare(&s);
    let row = &s.ledgers[0];
    let old = store::StoredFileMarker {
        file_id: row.file.file_id,
        version: row.file.version.clone(),
        unit: row.units[0],
        intro_start_ms: Some(30_000),
        intro_end_ms: Some(90_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "fingerprint".into(),
        locked: false,
        updated_at: 1,
    };
    s.store.put_fixture_marker(&old).unwrap();
    let before = s
        .store
        .current_chapter_frame_task(row.file.file_id)
        .unwrap()
        .unwrap()
        .task;
    let engine = Arc::new(PerWindow);
    s.manager = Arc::new(
        api::probe_manager::ProbeManager::with_engines(s.store.clone(), engine.clone(), engine)
            .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    assert_eq!(wait(&s).await.status, "succeeded");
    let after = s
        .store
        .get_file_marker(old.file_id, old.unit)
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (old.intro_start_ms, old.intro_end_ms),
        "1600ms outer pair disagreement must preserve the old marker"
    );
    assert_eq!(
        s.store
            .current_chapter_frame_task(old.file_id)
            .unwrap()
            .unwrap()
            .task,
        before
    );
    let unit = s
        .store
        .probe_job_units(&job)
        .unwrap()
        .into_iter()
        .find(|u| u.ledger_id == old.file_id.to_string())
        .unwrap();
    let outcome: serde_json::Value =
        serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
    assert!(
        outcome["kinds"]["intro"]["reason"]
            .as_str()
            .unwrap()
            .contains("conflicting_verified_windows")
    );
}
