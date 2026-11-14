use super::*;

struct TargetMismatch {
    path: std::path::PathBuf,
}
impl marker::FingerprintCaptureEngine for TargetMismatch {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        assert_eq!(
            request.path, self.path,
            "fallback must never reopen a reference source"
        );
        let mut capture =
            marker::FingerprintCaptureEngine::capture_window(&SharedMatchEngine, request)?;
        capture.words[0] = u32::MAX;
        Ok(capture)
    }
}
impl marker::FingerprintEngine for TargetMismatch {
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
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Option<marker::CommonSegment> {
        if first.first() == Some(&u32::MAX) || second.first() == Some(&u32::MAX) {
            return None;
        }
        marker::FingerprintEngine::find_common_segment(&SharedMatchEngine, first, second, min, max)
    }
}

#[tokio::test]
async fn unclassified_engine_result_stops_without_range_expansion_and_preserves_references() {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for episode in 1..=3 {
        let row = row(&store, media, &root, 1, episode);
        manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        rows.push(row);
    }
    drain(&store, &manager, &clock, &rows).await;
    prime_seed_results(&store, &manager, &clock, &rows).await;
    let before = rows
        .iter()
        .map(|row| {
            store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        before.iter().all(Option::is_some),
        "prime published seed results before testing preservation"
    );
    let model_references = model_reference_ids(&store, media);
    manager.shutdown_workers().await.unwrap();
    let target = row(&store, media, &root, 1, 4);
    let engine = Arc::new(TargetMismatch {
        path: target.file.path.clone(),
    });
    let manager =
        Arc::new(ProbeManager::with_engines(store.clone(), engine.clone(), engine).unwrap());
    manager.set_clock(clock.clone());
    manager.try_start_workers();
    let job = manager
        .enqueue_direct_fingerprint(
            unit(target.clone()),
            Some(1_800_000),
            target.file.version.source_version.clone(),
            180,
        )
        .unwrap();
    rows.push(target.clone());
    drain(&store, &manager, &clock, &rows).await;
    let attempts = store.list_fingerprint_attempts_for_job(&job).unwrap();
    // Candidate-only engines do not establish absence. An unclassified comparison
    // is not evidence that another source range should be read.
    for (kind, expected) in [("intro", (0, 100_000)), ("outro", (1_620_000, 1_720_000))] {
        let windows = attempts
            .iter()
            .filter(|a| a.kind == kind)
            .map(|a| (a.window_start_ms, a.window_end_ms))
            .collect::<Vec<_>>();
        assert_eq!(
            windows,
            vec![expected],
            "unclassified {kind} must not expand: {attempts:?}"
        );
    }
    let terminal = &store.probe_job_units(&job).unwrap()[0];
    assert_eq!(terminal.status, "succeeded");
    let outcome: serde_json::Value =
        serde_json::from_str(terminal.detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(outcome["status"], "uncertain");
    assert!(
        outcome["kinds"]["intro"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("unusable_reference_fingerprint"))
    );
    assert_eq!(
        rows[..3]
            .iter()
            .map(|row| store
                .get_file_marker(row.file.file_id, row.units[0])
                .unwrap())
            .collect::<Vec<_>>(),
        before
    );
    assert_eq!(model_reference_ids(&store, media), model_references);
    clock.set(clock.now_ms() + 10_000_000);
    manager.wake_dispatch();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        store
            .latest_probe_job_for_ledger(&target.file.file_id.to_string())
            .unwrap(),
        Some(job.clone())
    );
    assert_eq!(
        store.list_fingerprint_attempts_for_job(&job).unwrap().len(),
        attempts.len()
    );
    manager.shutdown_workers().await.unwrap();
}

fn model_reference_ids(store: &Store, media: MediaId) -> Vec<(String, Vec<String>)> {
    let mut models = store
        .list_fingerprint_models(&media.to_string(), 1)
        .unwrap()
        .into_iter()
        .map(|model| {
            let bundle = store
                .load_fingerprint_model_bundle(&model.model_id)
                .unwrap()
                .unwrap();
            let mut samples = bundle
                .samples
                .iter()
                .map(|sample| sample.sample_id.clone())
                .collect::<Vec<_>>();
            samples.sort();
            (model.model_id, samples)
        })
        .collect::<Vec<_>>();
    models.sort();
    models
}

#[tokio::test]
async fn explicit_recapture_reviews_new_target_samples_instead_of_old_full_windows() {
    recapture_reviews_new_target_samples(true).await;
}

#[tokio::test]
async fn ordinary_recapture_reviews_new_target_samples_instead_of_old_model_members() {
    recapture_reviews_new_target_samples(false).await;
}

async fn recapture_reviews_new_target_samples(refresh: bool) {
    let (_temp, store, manager, clock, media, root) = setup();
    let mut rows = Vec::new();
    for episode in 1..=3 {
        let row = row(&store, media, &root, 1, episode);
        manager
            .enqueue_direct_fingerprint(
                unit(row.clone()),
                Some(1_800_000),
                row.file.version.source_version.clone(),
                180,
            )
            .unwrap();
        rows.push(row);
    }
    drain(&store, &manager, &clock, &rows).await;
    prime_seed_results(&store, &manager, &clock, &rows).await;
    let target = &rows[0];
    let previous = store
        .get_file_marker(target.file.file_id, target.units[0])
        .unwrap();
    assert!(previous.is_some());
    manager.shutdown_workers().await.unwrap();
    let engine = Arc::new(TargetMismatch {
        path: target.file.path.clone(),
    });
    let manager =
        Arc::new(ProbeManager::with_engines(store.clone(), engine.clone(), engine).unwrap());
    manager.set_clock(clock.clone());
    manager.try_start_workers();
    if refresh {
        assert_eq!(
            manager.enqueue_marker_refresh(vec![unit(target.clone())]),
            1
        );
    } else {
        let mut target_unit = unit(target.clone());
        target_unit.reuse_fingerprint_cache = false;
        manager
            .enqueue_direct_fingerprint(
                target_unit,
                Some(1_800_000),
                target.file.version.source_version.clone(),
                180,
            )
            .unwrap();
    }
    let job = store
        .active_probe_unit_for_ledger(&target.file.file_id.to_string())
        .unwrap()
        .unwrap()
        .job_id;
    drain(&store, &manager, &clock, std::slice::from_ref(target)).await;
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded",
        "new recapture cannot publish an old cached match"
    );
    let units = store.probe_job_units(&job).unwrap();
    let outcome: serde_json::Value =
        serde_json::from_str(units[0].detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(outcome["status"], "uncertain");
    assert_eq!(
        store
            .get_file_marker(target.file.file_id, target.units[0])
            .unwrap(),
        previous
    );
    manager.shutdown_workers().await.unwrap();
}

async fn prime_seed_results(
    store: &Store,
    manager: &ProbeManager,
    clock: &FakeClock,
    rows: &[FileCoverage],
) {
    assert_eq!(
        manager.enqueue_marker_refresh_with_policy(
            rows.iter().cloned().map(unit).collect(),
            api::fingerprint_job::CapturePolicy::ReuseValid,
        ),
        rows.len()
    );
    let job = store
        .active_probe_unit_for_ledger(&rows[0].file.file_id.to_string())
        .unwrap()
        .unwrap()
        .job_id;
    drain(store, manager, clock, rows).await;
    assert_eq!(
        store.get_probe_job(&job).unwrap().unwrap().status,
        "succeeded"
    );
    assert!(rows.iter().all(|row| {
        store
            .get_file_marker(row.file.file_id, row.units[0])
            .unwrap()
            .is_some()
    }));
}
