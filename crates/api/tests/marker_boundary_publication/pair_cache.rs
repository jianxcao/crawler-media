use super::*;

fn publication_snapshot(s: &ProbeScenario, job: &store::ProbeJob) -> serde_json::Value {
    let units = s.store.probe_job_units(&job.id).unwrap();
    let files = s.ledgers.iter().map(|row| {
        let id = row.file.file_id.to_string();
        let marker = s.store.get_file_marker(row.file.file_id, row.units[0]).unwrap().unwrap();
        let unit = units.iter().find(|u| u.ledger_id == id).unwrap();
        let outcome = unit.detection_outcome_json.as_deref().map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap());
        serde_json::json!({
            "file_id": id, "unit": row.units[0], "source": marker.source, "locked": marker.locked,
            "intro": [marker.intro_start_ms, marker.intro_end_ms],
            "outro": [marker.outro_start_ms, marker.outro_end_ms],
            "chapters": s.store.get_cached_chapters(&id).unwrap(), "outcome": outcome,
        })
    }).collect::<Vec<_>>();
    serde_json::json!({"status": job.status, "completed": job.completed, "failed": job.failed, "files": files})
}

async fn assert_cached_and_uncached_publication(mode: &str) {
    use api::ScrapeStoreExt;
    let mut s = scenario().await;
    let mut config = s.store.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some(mode.into());
    s.store.save_scrape_config(&config).unwrap();
    s.manager.shutdown_workers().await.unwrap();
    let mut snapshots = Vec::new();
    let mut contexts = Vec::new();
    for (max_pairs, max_bytes) in [(10_000, 32 * 1024 * 1024), (0, 0)] {
        let session = Arc::new(marker::matcher::session::SeasonMatchSession::new(
            Arc::new(FixedCandidate),
            max_pairs,
            max_bytes,
        ));
        s.manager = Arc::new(
            ProbeManager::with_comparison_session(
                s.store.clone(),
                session,
                Arc::new(FixedCandidate),
            )
            .unwrap(),
        );
        s.manager.set_clock(s.clock.clone());
        assert!(s.manager.try_start_workers());
        let job = refresh(&s).await;
        assert_eq!(job.status, "succeeded", "{mode}: {job:?}");
        snapshots.push(publication_snapshot(&s, &job));
        let row = s
            .store
            .file_coverage_by_id(s.ledgers[0].file.file_id)
            .unwrap()
            .unwrap();
        let policy = marker::SamplingPolicy::default();
        contexts.push(
            s.store
                .prepare_probe_analysis(&store::ProbeAnalysisRequest {
                    dispatch: store::ProbeDispatchId {
                        job_id: job.id.clone(),
                        ledger_id: row.file.file_id.to_string(),
                        stage: store::ProbeDispatchStage::Analysis,
                    },
                    target: row,
                    season: 1,
                    sampling_mode: mode.into(),
                    analysis_policy_key: api::fingerprint_job::analysis_policy_key(&policy),
                    analysis_policy_json: serde_json::to_string(&policy).unwrap(),
                    capture_context_key: String::new(),
                    required_kinds: vec![],
                })
                .unwrap()
                .context()
                .clone(),
        );
        let work = s.manager.timing_snapshot(&job.id).unwrap();
        assert!(work.raw_pairs_computed.unwrap() > 0);
        if max_pairs == 0 {
            assert_eq!(work.raw_pairs_reused, Some(0));
        } else {
            assert!(work.raw_pairs_reused.unwrap() > 0);
        }
        s.manager.shutdown_workers().await.unwrap();
    }
    assert_eq!(
        contexts[0], contexts[1],
        "identical capture reuse must retain semantic input context"
    );
    assert_eq!(
        snapshots[0], snapshots[1],
        "{mode}: raw cache must not alter durable file outputs"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fullwindow_cache_on_and_off_publish_identical_file_results() {
    assert_cached_and_uncached_publication("full_window").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adaptive_cache_on_and_off_publish_identical_file_results() {
    assert_cached_and_uncached_publication("adaptive").await;
}

async fn ordinary(s: &ProbeScenario) -> store::ProbeJob {
    let row = &s.ledgers[0];
    assert!(s.manager.enqueue(unit(row, false)));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = s
                .store
                .latest_probe_job_for_scope(&format!("ledger:{}", row.file.file_id))
                .unwrap()
                .unwrap();
            if !job.is_active() && !s.manager.holds_dispatch_id(&row.file.file_id.to_string()) {
                return job;
            }
            advance_analysis(s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_reference_timeline_cannot_reuse_an_old_derived_season_decision() {
    let s = scenario().await;
    // Keep exactly two independent references: invalidating either one must
    // remove the support needed for a verified target, even with warm raw pairs.
    for row in &s.ledgers[3..] {
        s.store
            .delete_ledger_path(row.file.path.to_str().unwrap())
            .unwrap();
        std::fs::remove_file(&row.file.path).unwrap();
    }
    ordinary(&s).await;
    let before = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    let reference = s.ledgers[1].file.file_id.to_string();
    let mut cache = s.store.get_fingerprint_cache(&reference).unwrap().unwrap();
    cache.intro.metrics.pcm_timeline =
        marker::PcmTimeline::Invalid("observed_discontinuity".into());
    s.store.put_fingerprint_cache(&reference, &cache).unwrap();
    let changed = ordinary(&s).await;
    let units = s.store.probe_job_units(&changed.id).unwrap();
    assert!(
        units[0].detection_outcome_json.is_some(),
        "changed reference chronology must trigger current verification and a persisted typed outcome"
    );
    let outcome: serde_json::Value =
        serde_json::from_str(units[0].detection_outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(outcome["status"], "partial");
    assert_eq!(outcome["kinds"]["intro"]["status"], "uncertain");
    assert_eq!(outcome["kinds"]["outro"]["status"], "verified");
    assert!(
        outcome["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
    let work = s.manager.timing_snapshot(&changed.id).unwrap();
    assert_eq!(work.raw_pairs_computed, Some(0));
    assert!(work.raw_pairs_reused.unwrap() > 0);
    let after = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (before.intro_start_ms, before.intro_end_ms)
    );
    s.manager.shutdown_workers().await.unwrap();
}

pub(super) async fn refresh(s: &ProbeScenario) -> store::ProbeJob {
    assert_eq!(
        s.manager.enqueue_marker_refresh_with_policy(
            s.ledgers.iter().map(|r| unit(r, true)).collect(),
            api::fingerprint_job::CapturePolicy::ReuseValid
        ),
        s.ledgers.len()
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = s
                .store
                .latest_marker_refresh_for_season(&s.media_id.to_string(), 1)
                .unwrap()
                && !job.is_active()
                && s.ledgers
                    .iter()
                    .all(|r| !s.manager.holds_dispatch_id(&r.file.file_id.to_string()))
            {
                return job;
            }
            advance_analysis(s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_reuses_raw_work_across_jobs_but_reviews_changed_chapters() {
    let s = scenario().await;
    let cold = refresh(&s).await;
    assert_eq!(cold.status, "succeeded");
    let cold_work = s.manager.timing_snapshot(&cold.id).unwrap();
    assert_eq!(cold_work.raw_pairs_computed, Some(112));
    assert!(cold_work.raw_pairs_reused.unwrap() >= 56);
    let before = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (before.intro_start_ms, before.intro_end_ms),
        (Some(30_000), Some(90_000))
    );
    let warm = refresh(&s).await;
    assert_eq!(warm.status, "succeeded");
    let warm_work = s.manager.timing_snapshot(&warm.id).unwrap();
    assert_eq!(warm_work.raw_pairs_computed, Some(0));
    assert_eq!(warm_work.stage_totals.compare, Some(0));
    assert!(warm_work.raw_pairs_reused.unwrap() >= 168);
    let same = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (same.intro_start_ms, same.intro_end_ms),
        (before.intro_start_ms, before.intro_end_ms)
    );
    // A source chapter without corroborating audio makes this target unresolved.
    // Warm raw evidence must not authorize a fresh publication or erase its old cut.
    s.store
        .put_cached_chapters(
            &s.ledgers[0].file.file_id.to_string(),
            &[marker::ChapterMarker {
                start_ms: 31_000,
                end_ms: 89_000,
                title: Some("source chapter".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    let changed = refresh(&s).await;
    assert_eq!(changed.status, "succeeded");
    let outcome = latest_outcome(&s);
    assert_eq!(outcome["status"], "uncertain");
    assert!(
        outcome["reason"]
            .as_str()
            .unwrap()
            .contains("verification_unresolved")
    );
    let changed_work = s.manager.timing_snapshot(&changed.id).unwrap();
    assert_eq!(changed_work.raw_pairs_computed, Some(0));
    assert!(changed_work.raw_pairs_reused.unwrap() >= 168);
    let after = s
        .store
        .get_fixture_marker(s.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.intro_start_ms, after.intro_end_ms),
        (before.intro_start_ms, before.intro_end_ms)
    );
    s.manager.shutdown_workers().await.unwrap();
}
