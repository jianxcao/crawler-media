use super::*;
use api::probe_manager::ProbeManager;
use std::sync::Arc;

pub(super) async fn recovered_job(
    s: &mut support::ProbeScenario,
    engine: &Arc<LateEngine>,
    plan: &str,
) -> String {
    s.manager.shutdown_workers().await.unwrap();
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
    let job = "recovered-capture-job";
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
    for id in &ids {
        assert!(s.store.put_probe_sampling_plan(job, id, plan).unwrap());
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), engine.clone()).unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    job.into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_or_unsupported_persistent_capture_plan_finishes_without_io() {
    for plan in [
        "{broken",
        r#"{"fallback_kinds":[],"selected_samples":[],"policy":{},"sampling_mode":"adaptive"}"#,
    ] {
        let (mut s, engine) = scenario(1, 330_000, 410_000, None).await;
        let id = recovered_job(&mut s, &engine, plan).await;
        let job = wait(&s).await;
        assert_eq!(job.id, id);
        assert_eq!(job.status, "failed");
        let units = s.store.probe_job_units(&id).unwrap();
        assert_eq!(units.len(), 1);
        assert!(
            units[0]
                .error
                .as_deref()
                .is_some_and(|reason| reason.contains("invalid_capture_plan")),
            "{units:?}"
        );
        assert!(engine.requests.lock().unwrap().is_empty());
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_current_job_sample_is_selected_after_restart_without_recapture() {
    let (mut s, engine) = scenario(3, 330_000, 410_000, None).await;
    s.manager.shutdown_workers().await.unwrap();
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
    let job = "committed-before-selection";
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
        evidence::put_sample(
            &s,
            row,
            &format!("initial-{index}"),
            marker::SampleWindow::new(0, 180_000),
            job,
        );
        evidence::put_sample(
            &s,
            row,
            &format!("old-enclosing-{index}"),
            marker::SampleWindow::new(300_000, 600_000),
            "old-job",
        );
        evidence::put_sample(
            &s,
            row,
            &format!("committed-late-{index}"),
            marker::SampleWindow::new(320_000, 420_000),
            job,
        );
        evidence::put_sample(
            &s,
            row,
            &format!("newer-global-{index}"),
            marker::SampleWindow::new(300_000, 600_000),
            "old-job",
        );
        let plan = serde_json::json!({"searches":[
            {"kind":"intro","windows":[{"start_ms":0,"end_ms":180_000}],"pending":{"start_ms":320_000,"end_ms":420_000},"terminal_reason":null},
            {"kind":"outro","windows":[],"pending":{"start_ms":2_220_000,"end_ms":2_400_000},"terminal_reason":null}
        ],"selected_samples":[format!("initial-{index}")],"policy":marker::SamplingPolicy { max_windows_per_kind:3, ..Default::default() },"sampling_mode":"adaptive"});
        assert!(
            s.store
                .put_probe_sampling_plan(job, &row.file.file_id.to_string(), &plan.to_string())
                .unwrap()
        );
    }
    s.manager = Arc::new(
        ProbeManager::with_engines(s.store.clone(), engine.clone(), engine.clone()).unwrap(),
    );
    s.manager.set_clock(s.clock.clone());
    assert!(s.manager.try_start_workers());
    let complete = wait(&s).await;
    assert_eq!(complete.status, "succeeded");
    assert_eq!(
        engine.requests.lock().unwrap().len(),
        3,
        "only previously pending Outro may be captured"
    );
    for (index, row) in s.ledgers.iter().enumerate() {
        let marker = s
            .store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (marker.intro_start_ms, marker.intro_end_ms),
            (Some(330_000), Some(410_000))
        );
        let unit = s
            .store
            .probe_job_units(job)
            .unwrap()
            .into_iter()
            .find(|u| u.ledger_id == row.file.file_id.to_string())
            .unwrap();
        let plan: serde_json::Value =
            serde_json::from_str(unit.sampling_plan_json.as_deref().unwrap()).unwrap();
        assert!(
            plan["selected_samples"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(format!("committed-late-{index}")))
        );
        assert_eq!(
            s.store
                .list_fingerprint_attempts_for_job(job)
                .unwrap()
                .iter()
                .filter(|a| a.ledger_id == row.file.file_id.to_string() && a.kind == "intro")
                .count(),
            2
        );
    }
}
