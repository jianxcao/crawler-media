use super::*;
use api::fingerprint_job::{CapturePolicy, capture_profile_key, profile_for_source};

pub(super) fn put_sample(
    s: &support::ProbeScenario,
    row: &domain::FileCoverage,
    id: &str,
    window: marker::SampleWindow,
    job: &str,
) {
    let capture = support::fixture_capture_window(
        window.clone(),
        vec![window.start_ms as u32, window.end_ms as u32, 7, 19],
    );
    let attempt = store::StoredFingerprintAttempt {
        attempt_id: format!("attempt-{id}"),
        job_id: job.into(),
        ledger_id: row.file.file_id.to_string(),
        kind: "intro".into(),
        window_start_ms: window.start_ms,
        window_end_ms: window.end_ms,
        phase: "fixture".into(),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        status: "succeeded".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    s.store.begin_fingerprint_attempt(&attempt).unwrap();
    s.store
        .complete_fingerprint_attempt(
            &attempt,
            Some(&store::StoredFingerprintSample {
                sample_id: id.into(),
                ledger_id: row.file.file_id.to_string(),
                source_version: row.file.version.source_version.clone(),
                capture_profile_key: capture_profile_key(&profile_for_source(Some(
                    &support::fixture_source(),
                ))),
                kind: "intro".into(),
                window_start_ms: window.start_ms,
                window_end_ms: window.end_ms,
                pcm_duration_ms: capture.pcm_duration_ms,
                fingerprint: capture.words,
                captured_job_id: job.into(),
                captured_at_ms: 2,
                metrics_json: serde_json::to_string(&capture.metrics).unwrap(),
            }),
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_long_capture_cannot_replace_the_bounded_target_even_with_reuse() {
    let (s, engine) = scenario(3, 330_000, 410_000, None).await;
    for (n, row) in s.ledgers.iter().enumerate() {
        put_sample(
            &s,
            row,
            &format!("old600-{n}"),
            marker::SampleWindow::new(0, 600_000),
            "old-job",
        );
    }
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ),
        3
    );
    let job = wait(&s).await;
    assert_eq!(job.status, "succeeded");
    assert_windows(&s, &engine, &[(0, 180_000), (120_000, 420_000)]);
    for unit in s.store.probe_job_units(&job.id).unwrap() {
        let plan: serde_json::Value =
            serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
        assert!(
            plan["selected_samples"]
                .as_array()
                .unwrap()
                .iter()
                .all(|id| !id.as_str().unwrap().starts_with("old600"))
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stable_late_model_survives_outro_rebuild_and_reuses_actual_cached_window() {
    let (s, engine) = scenario(3, 330_000, 410_000, None).await;
    let first = run(&s).await;
    assert_eq!(first.status, "succeeded");
    let reads = engine.requests.lock().unwrap().len();
    // Reopen through the same public constructor; all model/sample facts come from SQLite.
    let manager = std::sync::Arc::new(
        api::probe_manager::ProbeManager::with_engines(
            s.store.clone(),
            engine.clone(),
            engine.clone(),
        )
        .unwrap(),
    );
    manager.set_clock(s.clock.clone());
    assert!(manager.try_start_workers());
    let mut s = s;
    s.manager = manager;
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(unit).collect(),
            CapturePolicy::ReuseValid
        ),
        3
    );
    let second = wait(&s).await;
    assert_eq!(second.status, "succeeded");
    assert_eq!(engine.requests.lock().unwrap().len(), reads);
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
    let models = s
        .store
        .fingerprint_models_page(
            &s.media_id.to_string(),
            1,
            "intro",
            &api::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy {
                max_windows_per_kind: 3,
                ..Default::default()
            }),
            None,
            3,
        )
        .unwrap();
    assert!(!models.is_empty());
    for model in models {
        let bundle = s
            .store
            .load_fingerprint_model_bundle(&model.model_id)
            .unwrap()
            .unwrap();
        assert_eq!(bundle.samples.len(), 3);
        assert!(
            bundle
                .samples
                .iter()
                .all(|sample| (sample.window_start_ms, sample.window_end_ms) == (120_000, 420_000))
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_unknown_leading_wrong_source_and_short_pcm_do_not_expand() {
    for fault in ["invalid", "unknown", "leading", "source", "incomplete"] {
        let (s, engine) = scenario(3, -1, 0, None).await;
        *engine.fault.lock().unwrap() = Some(fault);
        let job = run(&s).await;
        assert_eq!(job.status, "succeeded", "fault={fault}");
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
async fn one_late_episode_matches_two_early_independent_theme_references() {
    let (s, engine) = scenario(3, 330_000, 410_000, None).await;
    engine
        .mixed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let job = run(&s).await;
    assert_eq!(job.status, "succeeded");
    for (index, row) in s.ledgers.iter().enumerate() {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .expect("one late episode and two early episodes must form one stable theme");
        let expected = if index == 0 {
            (Some(330_000), Some(410_000))
        } else {
            (Some(30_000), Some(110_000))
        };
        assert_eq!((marker.intro_start_ms, marker.intro_end_ms), expected);
    }
    let models = s
        .store
        .fingerprint_models_page(
            &s.media_id.to_string(),
            1,
            "intro",
            &api::fingerprint_job::analysis_policy_key(&marker::SamplingPolicy {
                max_windows_per_kind: 3,
                ..Default::default()
            }),
            None,
            3,
        )
        .unwrap();
    assert!(!models.is_empty());
    for model in models {
        let bundle = s
            .store
            .load_fingerprint_model_bundle(&model.model_id)
            .unwrap()
            .unwrap();
        assert_eq!(bundle.samples.len(), 3);
        let distinct = bundle
            .samples
            .iter()
            .map(|s| &s.ledger_id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct.len(), 3);
        for sample in &bundle.samples {
            assert_eq!(
                (sample.window_start_ms, sample.window_end_ms),
                if sample.ledger_id == s.ledgers[0].file.file_id.to_string() {
                    (120_000, 420_000)
                } else {
                    (0, 180_000)
                }
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stable_model_predicts_only_the_next_files_late_window() {
    use api::probe_manager::clock::ProbeClock;
    let (mut s, engine) = scenario(3, 330_000, 410_000, None).await;
    assert_eq!(run(&s).await.status, "succeeded");
    let next = s
        .store
        .analysis_season_files_page(s.media_id, 1, None, 16)
        .unwrap()
        .into_iter()
        .find(|r| r.units.iter().any(|u| u.episode == 4))
        .unwrap();
    s.manager = std::sync::Arc::new(
        api::probe_manager::ProbeManager::with_engines(
            s.store.clone(),
            engine.clone(),
            engine.clone(),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    assert!(s.manager.enqueue(unit(&next)));
    let scope = format!("ledger:{}", next.file.file_id);
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = s.store.latest_probe_job_for_scope(&scope).unwrap()
                && !job.is_active()
            {
                break job;
            }
            s.clock.set(s.clock.now_ms() + 2_000);
            s.manager.wake_dispatch();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    s.manager.shutdown_workers().await.unwrap();
    assert_eq!(job.status, "succeeded");
    let marker = s
        .store
        .get_file_marker(next.file.file_id, next.units[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        (marker.intro_start_ms, marker.intro_end_ms),
        (Some(330_000), Some(410_000))
    );
    let windows = engine
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p == &next.file.path.to_string_lossy())
        .map(|(_, w)| (w.start_ms, w.end_ms))
        .collect::<Vec<_>>();
    assert_eq!(windows, vec![(320_000, 420_000), (2_220_000, 2_400_000)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn already_verified_early_intros_are_not_read_for_an_unresolved_late_file() {
    for (start, end) in [(330_000, 410_000), (450_000, 530_000)] {
        let (s, engine) = scenario(8, start, end, None).await;
        engine
            .mixed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let job = run(&s).await;
        assert_eq!(job.status, "succeeded");
        for (index, row) in s.ledgers.iter().enumerate() {
            let marker = s
                .store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .unwrap();
            assert_eq!(
                (marker.intro_start_ms, marker.intro_end_ms),
                if index == 0 {
                    (Some(start), Some(end))
                } else {
                    (Some(30_000), Some(110_000))
                }
            );
            let windows = engine
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, w)| p == &row.file.path.to_string_lossy() && w.start_ms < 600_000)
                .map(|(_, w)| (w.start_ms, w.end_ms))
                .collect::<Vec<_>>();
            assert_eq!(
                windows,
                if index == 0 {
                    if start < 420_000 {
                        vec![(0, 180_000), (120_000, 420_000)]
                    } else {
                        vec![(0, 180_000), (120_000, 420_000), (300_000, 600_000)]
                    }
                } else {
                    vec![(0, 180_000)]
                }
            );
        }
    }
}
