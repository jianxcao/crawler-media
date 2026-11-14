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
#[path = "marker_late_intro_capture/mixed_reuse.rs"]
mod mixed_reuse;
#[path = "marker_late_intro_capture/review_conflict.rs"]
mod review_conflict;
#[path = "marker_late_intro_capture/review_mapping.rs"]
mod review_mapping;
#[path = "marker_late_intro_capture/review_plans.rs"]
mod review_plans;
