use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
fn plan() -> Value {
    json!({"searches":[{"kind":"intro","windows":[],"pending":{"start_ms":0,"end_ms":180_000},"terminal_reason":null}],"selected_samples":[],"policy":marker::SamplingPolicy {max_windows_per_kind:3,..Default::default()},"sampling_mode":"adaptive"})
}
async fn rejected(plan: Value) {
    let (mut s, engine) = scenario(1, 330_000, 410_000, None).await;
    s.store
        .delete_fingerprint_cache(&s.ledgers[0].file.file_id.to_string())
        .unwrap();
    // The old bug's first forbidden physical read terminates the fixture, so RED
    // observes actual wrong IO instead of waiting through an unbounded retry loop.
    let store = s.store.clone();
    let id = s.ledgers[0].file.file_id.to_string();
    *engine.on_capture.lock().unwrap() = Some(Arc::new(move |_| {
        if let Some(unit) = store.active_probe_unit_for_ledger(&id).unwrap() {
            store
                .cancel_probe_job(&unit.job_id, "forbidden invalid-plan IO observed")
                .unwrap();
        }
    }));
    let id = recovery::recovered_job(&mut s, &engine, &plan.to_string()).await;
    let job = wait(&s).await;
    assert_eq!(
        job.status, "failed",
        "current-shaped unsupported plan must fail before IO: {plan}"
    );
    assert!(engine.requests.lock().unwrap().is_empty());
    let unit = &s.store.probe_job_units(&id).unwrap()[0];
    assert!(
        unit.error
            .as_deref()
            .is_some_and(|reason| reason.contains("invalid_capture_plan")),
        "{unit:?}"
    );
    assert!(
        s.store
            .get_file_marker(s.ledgers[0].file.file_id, s.ledgers[0].units[0])
            .unwrap()
            .is_none()
    );
    assert!(
        s.store
            .current_chapter_frame_task(s.ledgers[0].file.file_id)
            .unwrap()
            .is_none()
    );
    assert!(s.store.recover_probe_jobs().unwrap().is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_current_shaped_plan_mode_is_terminal_before_io() {
    let mut p = plan();
    p["sampling_mode"] = json!("adaptive_v2");
    rejected(p).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_intro_outside_search_or_invalid_geometry_is_terminal_before_io() {
    for (start, end) in [
        (600_000, 900_000),
        (-1, 180_000),
        (180_000, 180_000),
        (0, 300_001),
    ] {
        let mut p = plan();
        p["searches"][0]["pending"] = json!({"start_ms":start,"end_ms":end});
        rejected(p).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_adaptive_quota_policy_is_terminal_before_io() {
    for (field, value) in [
        ("max_windows_per_kind", 4),
        ("max_attempts_per_kind", 5),
        ("process_deadline_ms", 120_001),
        ("full_window_duration_secs", 600),
        ("seed_count", 1),
        ("max_seed_count", 6),
    ] {
        let mut p = plan();
        p["policy"][field] = json!(value);
        rejected(p).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contradictory_search_state_is_terminal_before_io() {
    for state in [
        "duplicate_kind",
        "duplicate_window",
        "terminal_pending",
        "selected_without_search",
    ] {
        let mut p = plan();
        match state {
            "duplicate_kind" => {
                let entry = p["searches"][0].clone();
                p["searches"].as_array_mut().unwrap().push(entry);
            }
            "duplicate_window" => {
                p["searches"][0]["windows"] =
                    json!([{"start_ms":0,"end_ms":180_000},{"start_ms":0,"end_ms":180_000}])
            }
            "terminal_pending" => p["searches"][0]["terminal_reason"] = json!("already exhausted"),
            _ => {
                p["searches"] = json!([]);
                p["selected_samples"] = json!(["unbound-sample"]);
            }
        }
        rejected(p).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supported_full_mode_custom_duration_ignores_unused_adaptive_quotas() {
    let (mut s, engine) = scenario(1, -1, 0, None).await;
    let p = json!({"searches":[],"selected_samples":[],"policy":marker::SamplingPolicy {full_window_duration_secs:90,max_windows_per_kind:0,max_attempts_per_kind:0,seed_count:0,max_seed_count:0,..Default::default()},"sampling_mode":"full_window"});
    recovery::recovered_job(&mut s, &engine, &p.to_string()).await;
    let job = wait(&s).await;
    assert_eq!(job.status, "succeeded");
    let requests = engine
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, w)| (w.start_ms, w.end_ms))
        .collect::<Vec<_>>();
    assert_eq!(requests, vec![(0, 90_000), (2_310_000, 2_400_000)]);
}

struct CorruptAtAnalysis {
    store: Arc<store::Store>,
    ledger: String,
    changed: std::sync::atomic::AtomicBool,
}
impl marker::FingerprintEngine for CorruptAtAnalysis {
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
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
    fn compare_fingerprints(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> marker::PairFingerprintDecision {
        if !self.changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let unit = self
                .store
                .active_probe_unit_for_ledger(&self.ledger)
                .unwrap()
                .unwrap();
            assert_eq!(unit.stage, store::ProbeDispatchStage::Analysis);
            let mut plan: Value =
                serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
            plan["sampling_mode"] = json!("adaptive_v2");
            assert!(
                self.store
                    .put_probe_sampling_plan(&unit.job_id, &unit.ledger_id, &plan.to_string())
                    .unwrap()
            );
        }
        marker::PairFingerprintDecision::NoMatch
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_plan_rejection_after_handoff_is_terminal_in_analysis() {
    let (mut s, capture) = scenario(3, -1, 0, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let matcher = Arc::new(CorruptAtAnalysis {
        store: s.store.clone(),
        ledger: s.ledgers[0].file.file_id.to_string(),
        changed: Default::default(),
    });
    s.manager = Arc::new(
        api::probe_manager::ProbeManager::with_engines(
            s.store.clone(),
            matcher.clone(),
            capture.clone(),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    let job = run(&s).await;
    assert_eq!(job.status, "failed");
    assert!(matcher.changed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        s.store
            .probe_job_units(&job.id)
            .unwrap()
            .iter()
            .any(|unit| unit
                .error
                .as_deref()
                .is_some_and(|error| error.contains("invalid_capture_plan")))
    );
    assert_windows(&s, &capture, &[(0, 180_000)]);
    for row in &s.ledgers {
        assert!(
            s.store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_none()
        );
        assert!(
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .is_none()
        );
    }
    assert!(s.store.recover_probe_jobs().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_analysis_participant_releases_claim_so_valid_ordinary_jobs_drain() {
    use api::probe_manager::clock::ProbeClock;
    let (mut s, capture) = scenario(3, -1, 0, None).await;
    s.manager.shutdown_workers().await.unwrap();
    let matcher = Arc::new(CorruptAtAnalysis {
        store: s.store.clone(),
        ledger: s.ledgers[0].file.file_id.to_string(),
        changed: Default::default(),
    });
    s.manager = Arc::new(
        api::probe_manager::ProbeManager::with_engines(
            s.store.clone(),
            matcher.clone(),
            capture.clone(),
        )
        .unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    for row in &s.ledgers {
        assert!(s.manager.enqueue(unit(row)));
    }
    // Do not advance the fake quiet-time clock until all three ordinary jobs can
    // share a claim; this tests invalidating one participant of that claim.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let active = s.store.recover_probe_jobs().unwrap();
            if active.len() == 3
                && active
                    .iter()
                    .flat_map(|(_, units)| units)
                    .all(|unit| unit.stage == store::ProbeDispatchStage::Analysis)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !s.store.recover_probe_jobs().unwrap().is_empty() {
            s.clock.set(s.clock.now_ms() + 2_000);
            s.manager.wake_dispatch();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    s.manager.shutdown_workers().await.unwrap();
    assert!(matcher.changed.load(std::sync::atomic::Ordering::SeqCst));
    for (index, row) in s.ledgers.iter().enumerate() {
        let job = s
            .store
            .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
            .unwrap()
            .unwrap();
        assert_eq!(job.status, if index == 0 { "failed" } else { "succeeded" });
        let unit = &s.store.probe_job_units(&job.id).unwrap()[0];
        if index == 0 {
            assert!(
                unit.error
                    .as_deref()
                    .unwrap()
                    .contains("invalid_capture_plan")
            );
        } else {
            assert!(unit.detection_outcome_json.is_some());
        }
        assert!(
            s.store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
                .is_none()
        );
        assert!(
            s.store
                .current_chapter_frame_task(row.file.file_id)
                .unwrap()
                .is_none()
        );
    }
    let reads = capture.requests.lock().unwrap();
    for (index, row) in s.ledgers.iter().enumerate() {
        let windows = reads
            .iter()
            .filter(|(path, _)| path == &row.file.path.to_string_lossy())
            .map(|(_, w)| (w.start_ms, w.end_ms))
            .filter(|(start, _)| *start < 600_000)
            .collect::<Vec<_>>();
        if index == 0 {
            assert_eq!(windows, vec![(0, 180_000)]);
        } else {
            assert!(windows.len() <= 3);
            assert_eq!(
                windows
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                windows.len()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_window_rejects_zero_guard_evidence_policy_before_capture() {
    let mut p = json!({"searches":[],"selected_samples":[],"policy":marker::SamplingPolicy {full_window_duration_secs:90,max_windows_per_kind:0,max_attempts_per_kind:0,seed_count:0,max_seed_count:0,..Default::default()},"sampling_mode":"full_window"});
    p["policy"]["boundary"]["min_guard_ms"] = json!(0);
    rejected(p).await;
}
