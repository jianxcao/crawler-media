#[path = "marker_late_intro_capture/support.rs"]
mod late;
#[path = "probe_retry_support.rs"]
mod support;
use late::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_episodes_discover_late_intro_and_publish_exact_samples() {
    let (s, engine) = scenario(3, 330_000, 410_000, None).await;
    let job = run(&s).await;
    assert_eq!(job.status, "succeeded");
    for row in &s.ledgers {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .expect("late Intro must publish from the second independent capture");
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            (Some(330_000), Some(410_000))
        );
        assert_eq!(marker.outro_start_ms, None);
        assert!(
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .is_some()
        );
        let unit = s
            .store
            .probe_job_units(&job.id)
            .unwrap()
            .into_iter()
            .find(|u| u.ledger_id == row.file.file_id.to_string())
            .unwrap();
        let outcome: serde_json::Value =
            serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
        assert_eq!(outcome["status"], "partial");
        assert_eq!(outcome["kinds"]["intro"]["status"], "verified");
        let plan: serde_json::Value =
            serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
        let ids = plan["selected_samples"].as_array().unwrap();
        let selected = ids
            .iter()
            .map(|id| {
                s.store
                    .fingerprint_sample_by_id(id.as_str().unwrap())
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(selected.iter().any(|sample| sample.kind == "intro"
            && sample.window_start_ms == 120_000
            && sample.window_end_ms == 420_000));
    }
    assert_windows(&s, &engine, &[(0, 180_000), (120_000, 420_000)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_candidate_exhausts_three_finite_windows_without_absence() {
    let (s, engine) = scenario(3, -1, 0, None).await;
    let job = run(&s).await;
    assert_eq!(
        job.status,
        "succeeded",
        "{job:?} units={:?}",
        s.store.probe_job_units(&job.id).unwrap()
    );
    assert_windows(
        &s,
        &engine,
        &[(0, 180_000), (120_000, 420_000), (300_000, 600_000)],
    );
    for unit in s.store.probe_job_units(&job.id).unwrap() {
        let result: serde_json::Value =
            serde_json::from_str(unit.detection_outcome_json.as_deref().unwrap()).unwrap();
        assert_eq!(result["kinds"]["intro"]["status"], "uncertain");
        assert!(
            result["kinds"]["intro"]["reason"]
                .as_str()
                .unwrap()
                .contains("sampling_limit")
        );
        assert!(
            s.store
                .get_file_marker(unit.ledger_id.parse().unwrap(), domain::UnitKey::new(1, 1))
                .unwrap()
                .is_none()
        );
        let attempts = s.store.list_fingerprint_attempts_for_job(&job.id).unwrap();
        assert_eq!(
            attempts
                .iter()
                .filter(|a| a.ledger_id == unit.ledger_id && a.kind == "intro")
                .count(),
            3
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insufficient_independent_episodes_do_not_expand() {
    for count in [1, 2] {
        let (s, engine) = scenario(count, 330_000, 410_000, None).await;
        let job = run(&s).await;
        assert_eq!(
            job.status,
            "succeeded",
            "{job:?} units={:?}",
            s.store.probe_job_units(&job.id).unwrap()
        );
        assert_windows(&s, &engine, &[(0, 180_000)]);
        assert!(s.ledgers.iter().all(|r| {
            s.store
                .get_file_marker(r.file.file_id, r.units[0])
                .unwrap()
                .is_none()
        }));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duration_clips_and_omits_covered_or_empty_windows() {
    for (duration, expected) in [
        (175_000, vec![(0, 175_000)]),
        (350_000, vec![(0, 180_000), (120_000, 350_000)]),
        (
            430_000,
            vec![(0, 180_000), (120_000, 420_000), (300_000, 430_000)],
        ),
    ] {
        let (s, engine) = scenario(3, -1, 0, Some(duration)).await;
        let job = run(&s).await;
        assert_eq!(
            job.status,
            "succeeded",
            "{job:?} units={:?}",
            s.store.probe_job_units(&job.id).unwrap()
        );
        for row in &s.ledgers {
            let requests = engine
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path == &row.file.path.to_string_lossy())
                .map(|(_, w)| (w.start_ms, w.end_ms))
                .collect::<Vec<_>>();
            // The initial tail is captured once; every later request belongs to Intro.
            let mut expected_all = vec![expected[0], ((duration - 180_000).max(0), duration)];
            expected_all.extend_from_slice(&expected[1..]);
            assert_eq!(requests, expected_all);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_duration_is_not_invented_and_skips_outro() {
    let (s, engine) = scenario(3, -1, 0, Some(0)).await;
    let job = run(&s).await;
    assert_eq!(
        job.status,
        "succeeded",
        "{job:?} units={:?}",
        s.store.probe_job_units(&job.id).unwrap()
    );
    assert_windows(
        &s,
        &engine,
        &[(0, 180_000), (120_000, 420_000), (300_000, 600_000)],
    );
    assert!(
        s.store
            .list_fingerprint_attempts_for_job(&job.id)
            .unwrap()
            .iter()
            .all(|a| a.kind == "intro")
    );
    for row in &s.ledgers {
        assert_eq!(
            s.store
                .get_media_info_cache_version(&row.file.file_id.to_string())
                .unwrap()
                .unwrap()
                .format_duration_ms,
            None
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seam_spanning_candidate_does_not_join_independent_pcm() {
    let (s, engine) = scenario(3, 250_000, 490_000, None).await;
    let job = run(&s).await;
    assert_eq!(
        job.status,
        "succeeded",
        "{job:?} units={:?}",
        s.store.probe_job_units(&job.id).unwrap()
    );
    assert_windows(
        &s,
        &engine,
        &[(0, 180_000), (120_000, 420_000), (300_000, 600_000)],
    );
    assert!(s.ledgers.iter().all(|r| {
        s.store
            .get_file_marker(r.file.file_id, r.units[0])
            .unwrap()
            .is_none()
    }));
}

#[path = "marker_late_intro_capture/budgets.rs"]
mod budgets;
#[path = "marker_late_intro_capture/evidence.rs"]
mod evidence;
#[path = "marker_late_intro_capture/recovery.rs"]
mod recovery;
#[path = "marker_late_intro_capture/source_guards.rs"]
mod source_guards;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_late_arrivals_revisit_exact_evidence_until_three_independent_episodes_exist() {
    use api::probe_manager::clock::ProbeClock;
    let (s, engine) = scenario(3, 330_000, 410_000, None).await;
    for (index, row) in s.ledgers.iter().enumerate() {
        assert!(s.manager.enqueue(unit(row)));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                s.clock.set(s.clock.now_ms() + 2_000);
                s.manager.wake_dispatch();
                if s.store.recover_probe_jobs().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if index < 2 {
            assert!(
                s.store
                    .get_file_marker(row.file.file_id, row.units[0])
                    .unwrap()
                    .is_none()
            );
            assert!(
                engine
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(_, w)| w.start_ms != 120_000)
            );
        }
    }
    s.manager.shutdown_workers().await.unwrap();
    for row in &s.ledgers {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            (Some(330_000), Some(410_000))
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_outro_clipped_at_default_180s_tail_expands_to_300s_and_publishes() {
    struct ClippedOutroEngine {
        requests: std::sync::Mutex<Vec<(String, marker::SampleWindow)>>,
    }
    impl marker::FingerprintEngine for ClippedOutroEngine {
        fn extract_at(
            &self,
            _: &std::path::Path,
            _: u32,
            _: u32,
            _: &marker::WorkControl,
        ) -> Result<Vec<u32>, String> {
            unreachable!()
        }
        fn find_common_segment(
            &self,
            a: &[u32],
            b: &[u32],
            min: f32,
            max: f32,
        ) -> Option<marker::CommonSegment> {
            // Intro at [20_000, 100_000]; early Outro at [2_190_000, 2_250_000] (150s post-credit + preview).
            for (s, e) in [(20_000i64, 100_000i64), (2_190_000i64, 2_250_000i64)] {
                let ov_s1 = s.max(a[0] as i64);
                let ov_e1 = e.min(a[1] as i64);
                let ov_s2 = s.max(b[0] as i64);
                let ov_e2 = e.min(b[1] as i64);
                let dur = (ov_e1 - ov_s1).min(ov_e2 - ov_s2);
                if dur as f32 / 1000.0 >= min && dur as f32 / 1000.0 <= max {
                    return Some(marker::CommonSegment {
                        start1_sec: (ov_s1 - a[0] as i64) as f32 / 1000.0,
                        end1_sec: (ov_e1 - a[0] as i64) as f32 / 1000.0,
                        start2_sec: (ov_s2 - b[0] as i64) as f32 / 1000.0,
                        end2_sec: (ov_e2 - b[0] as i64) as f32 / 1000.0,
                        duration_sec: dur as f32 / 1000.0,
                        score: 0.5,
                    });
                }
            }
            None
        }
        fn compare_fingerprints(
            &self,
            a: &[u32],
            b: &[u32],
            min: f32,
            max: f32,
        ) -> marker::PairFingerprintDecision {
            match self.find_common_segment(a, b, min, max) {
                Some(seg) => marker::PairFingerprintDecision::Candidates(vec![seg]),
                None => marker::PairFingerprintDecision::NoMatch,
            }
        }
    }
    impl marker::FingerprintCaptureEngine for ClippedOutroEngine {
        fn capture_window(
            &self,
            r: &marker::CaptureRequest,
        ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
            self.requests
                .lock()
                .unwrap()
                .push((r.path.to_string_lossy().into(), r.window.clone()));
            let ep = r
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
            Ok(support::fixture_capture_window(
                r.window.clone(),
                vec![r.window.start_ms as u32, r.window.end_ms as u32, ep, 42],
            ))
        }
    }

    let (mut s, _) = scenario(3, -1, 0, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let custom = std::sync::Arc::new(ClippedOutroEngine {
        requests: std::sync::Mutex::new(vec![]),
    });
    s.manager = std::sync::Arc::new(
        api::probe_manager::ProbeManager::with_engines(
            s.store.clone(),
            custom.clone(),
            custom.clone(),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());

    let job = run(&s).await;
    assert_eq!(job.status, "succeeded");
    for row in &s.ledgers {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .expect("both intro and early outro should publish");
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            (Some(20_000), Some(100_000))
        );
        assert_eq!(
            (marker.outro_start_ms, marker.outro_end_ms),
            (Some(2_190_000), Some(2_250_000))
        );
    }
}

#[path = "marker_late_intro_capture/mixed_reuse.rs"]
mod mixed_reuse;
#[path = "marker_late_intro_capture/review_conflict.rs"]
mod review_conflict;
#[path = "marker_late_intro_capture/review_mapping.rs"]
mod review_mapping;
#[path = "marker_late_intro_capture/review_plans.rs"]
mod review_plans;
