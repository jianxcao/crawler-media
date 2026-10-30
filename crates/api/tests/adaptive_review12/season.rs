use super::*;

struct ChangedCapture {
    inner: Arc<FixtureEngine>,
    store: Arc<Store>,
    target: String,
    mode: &'static str,
}

struct RestoreCancelledCapture {
    inner: Arc<FixtureEngine>,
    store: Arc<Store>,
    anchor: domain::LedgerId,
    triggered: std::sync::atomic::AtomicBool,
}

impl FingerprintCaptureEngine for RestoreCancelledCapture {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        let result = self.inner.capture_window(request)?;
        if !self.triggered.swap(true, Ordering::SeqCst) {
            let store = self.store.as_ref();
            for (presence, missing_at, at) in [
                (domain::FilePresence::Missing, None, 1_000),
                (domain::FilePresence::Present, Some(1_000), 1_001),
            ] {
                let file = store.file_coverage_by_id(self.anchor).unwrap().unwrap();
                let observation = store::LedgerPresenceObservation {
                    path_key: file.path_key.clone(),
                    file,
                    missing_at,
                    presence,
                };
                store
                    .apply_ledger_presence(&[observation], store::LedgerPresenceMode::Verify, at)
                    .unwrap();
            }
        }
        Ok(result)
    }
}

#[test]
fn restored_anchor_does_not_publish_adaptive_samples_after_cancellation() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        run_restored_anchor_capture_test();
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("adaptive sampling must complete without deadlock");
}

fn run_restored_anchor_capture_test() {
    let mut f = setup("uniform", &[1, 2, 3]);
    let ids: Vec<_> = f
        .units
        .iter()
        .map(|u| u.row.file.file_id.to_string())
        .collect();
    let capture = Arc::new(RestoreCancelledCapture {
        inner: f.engine.clone(),
        store: f.store.clone(),
        anchor: f.units[0].row.file.file_id,
        triggered: std::sync::atomic::AtomicBool::new(false),
    });
    f.manager = ProbeManager::with_engines(f.store.clone(), f.engine.clone(), capture).unwrap();
    {
        let store = f.store.as_ref();
        let specs: Vec<_> = ids
            .iter()
            .map(|id| store::ProbeJobUnitSpec {
                ledger_id: id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            })
            .collect();
        assert!(
            store
                .create_probe_job(
                    "adaptive-cancel",
                    "media_probe",
                    &f.media_id.to_string(),
                    Some(1),
                    "adaptive-cancel",
                    &specs
                )
                .unwrap()
        );
        for id in &ids {
            assert!(store.start_probe_unit("adaptive-cancel", id).unwrap());
        }
    }
    for unit in &mut f.units {
        unit.job_id = Some("adaptive-cancel".into());
        unit.overwrite_markers = false;
    }
    let result = tokio::runtime::Runtime::new().unwrap().block_on(
        api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager,
            "adaptive-cancel",
            f.media_id,
            1,
            &f.units,
        ),
    );
    let store = f.store.as_ref();
    let attempts = store
        .list_fingerprint_attempts_for_job("adaptive-cancel")
        .unwrap();
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted")),
        "pipeline result: {result:?}; attempts: {attempts:?}"
    );
    assert!(!attempts.is_empty());
    assert!(attempts.iter().all(|attempt| attempt.status == "cancelled"));
    let profile = api::fingerprint_job::capture_profile_key(
        &api::fingerprint_job::FingerprintCaptureProfile::default(),
    );
    for attempt in &attempts {
        let row = f
            .units
            .iter()
            .find(|unit| unit.row.file.file_id.to_string() == attempt.ledger_id)
            .unwrap();
        let samples = store
            .find_covering_fingerprint_samples(&store::FingerprintSampleQuery {
                ledger_id: attempt.ledger_id.clone(),
                source_version: api::fingerprint_job::current_source_version(&row.row.file.path),
                capture_profile_key: profile.clone(),
                kind: attempt.kind.clone(),
                window_start_ms: attempt.window_start_ms,
                window_end_ms: attempt.window_end_ms,
                captured_job_id: Some("adaptive-cancel".into()),
            })
            .unwrap();
        assert!(
            samples.is_empty(),
            "cancelled attempt persisted samples: {samples:?}"
        );
    }
}

impl FingerprintCaptureEngine for ChangedCapture {
    fn capture_window(&self, r: &CaptureRequest) -> Result<CapturedFingerprint, CaptureFailure> {
        let mut result = self.inner.capture_window(r)?;
        if r.path.to_str() == Some(&self.target) {
            if self.mode == "delete-outro" && r.window.start_ms > 0 {
                std::fs::remove_file(&r.path).unwrap();
                self.store
                    .as_ref()
                    .delete_ledger_path(&self.target)
                    .unwrap();
            } else if self.mode == "silent-intro" && r.window.start_ms == 0 {
                result.words = vec![0; 200];
            }
        }
        Ok(result)
    }
}
#[test]
fn final_outro_deletion_must_not_publish_deleted_episode() {
    let mut f = setup("uniform", &[1, 2, 3]);
    let row = f.units.last().unwrap().row.clone();
    let capture = Arc::new(ChangedCapture {
        inner: f.engine.clone(),
        store: f.store.clone(),
        target: row.file.path.to_string_lossy().into_owned(),
        mode: "delete-outro",
    });
    f.manager = ProbeManager::with_engines(f.store.clone(), f.engine.clone(), capture).unwrap();
    let result = raw_season(&f);
    println!(
        "after final outro deleted ledger={:?}; replacement={result:?}",
        f.store
            .as_ref()
            .file_coverage_by_id(row.file.file_id)
            .unwrap()
    );
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted")),
        "the final capture must recheck source membership before publishing a stale season"
    );
}
#[test]
fn inconclusive_silent_fingerprint_must_not_be_refreshed_as_success() {
    let mut f = setup("uniform", &[1, 2, 3]);
    let capture = Arc::new(ChangedCapture {
        inner: f.engine.clone(),
        store: f.store.clone(),
        target: f.units[0].row.file.path.to_string_lossy().into_owned(),
        mode: "silent-intro",
    });
    f.manager = ProbeManager::with_engines(f.store.clone(), f.engine.clone(), capture).unwrap();
    let result = raw_season(&f);
    if let Ok(rep) = &result {
        f.store
            .as_ref()
            .complete_marker_refresh("review9-job", std::slice::from_ref(rep))
            .unwrap();
        println!(
            "silent E1 source={:?}; job={:?}",
            rep.markers.iter().find(|m| m.episode == 1),
            f.store.as_ref().get_probe_job("review9-job").unwrap()
        );
    }
    assert!(
        result.is_err(),
        "a forced refresh cannot claim verified success when final intro remains constant_or_silent_fingerprint"
    );
}

#[test]
fn cancelled_last_unit_must_not_mark_aborted_forced_refresh_successful() {
    let mut f = setup("uniform", &[1, 2, 3]);
    let keys: Vec<_> = f
        .units
        .iter()
        .map(|u| u.row.file.file_id.to_string())
        .collect();
    {
        let st = f.store.as_ref();
        let specs: Vec<_> = keys
            .iter()
            .map(|k| api::store::ProbeJobUnitSpec {
                ledger_id: k,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
            })
            .collect();
        assert!(
            st.create_probe_job(
                "cancel-last",
                "marker_refresh",
                &f.media_id.to_string(),
                Some(1),
                "cancel-last-scope",
                &specs
            )
            .unwrap()
        );
        for key in &keys[..2] {
            st.start_probe_unit("cancel-last", key).unwrap();
            st.finish_probe_unit("cancel-last", key, true, None)
                .unwrap();
        }
    }
    for u in &mut f.units {
        u.job_id = Some("cancel-last".into());
    }
    let deleted = &f.units[2].row;
    std::fs::remove_file(&deleted.file.path).unwrap();
    f.store
        .as_ref()
        .delete_ledger_path(deleted.file.path.to_str().unwrap())
        .unwrap();
    let result = tokio::runtime::Runtime::new().unwrap().block_on(
        api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager,
            "cancel-last",
            f.media_id,
            1,
            &f.units,
        ),
    );
    let st = f.store.as_ref();
    let job = st.get_probe_job("cancel-last").unwrap().unwrap();
    println!(
        "aborted refresh result={result:?}; job={job:?}; units={:?}",
        st.probe_job_units("cancel-last").unwrap()
    );
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|e| e.starts_with("file_deleted"))
    );
    assert_ne!(
        job.status, "succeeded",
        "an aborted atomic refresh must not be terminalized as success by cancel_unit"
    );
}

#[test]
fn deletion_between_analysis_and_publication_keeps_old_results() {
    let f = setup("uniform", &[1, 2, 3]);
    let replacement = raw_season(&f).unwrap();
    let row = &f.units[2].row;
    std::fs::remove_file(&row.file.path).unwrap();
    f.store
        .as_ref()
        .delete_ledger_path(row.file.path.to_str().unwrap())
        .unwrap();
    let store = f.store.as_ref();
    let result = store.complete_marker_refresh_checked(
        "review9-job",
        std::slice::from_ref(&replacement),
        &observed(store, std::slice::from_ref(&replacement)),
    );
    assert!(result.is_err());
    let previous = store
        .get_media_marker(f.media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(previous.source, "old");
    assert!(
        store
            .get_cached_chapters(&row.file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert_ne!(
        store.get_probe_job("review9-job").unwrap().unwrap().status,
        "succeeded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_deletion_cancels_refresh_and_releases_scope_for_retry() {
    let f = setup_with_digest_versions("uniform", &[1, 2, 3]);
    let units = f.units.clone();
    let manager = Arc::new(f.manager);
    assert_eq!(manager.enqueue_marker_refresh(units.clone()), 3);
    let first_id = units[0].row.file.file_id.to_string();
    let job_id = f
        .store
        .as_ref()
        .active_probe_unit_for_ledger(&first_id)
        .unwrap()
        .unwrap()
        .job_id;
    std::fs::remove_file(&units[0].row.file.path).unwrap();
    f.store
        .as_ref()
        .delete_ledger_path(units[0].row.file.path.to_str().unwrap())
        .unwrap();
    assert!(manager.try_start_workers());
    let job = wait_for_terminal(&f.store, &job_id).await;
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.failed, 0);
    assert_eq!(job.completed, job.total);
    assert_eq!(f.engine.reads.load(Ordering::Relaxed), 0);
    assert_eq!(manager.enqueue_marker_refresh(units[1..].to_vec()), 2);
    let retry_id = f
        .store
        .as_ref()
        .active_probe_unit_for_ledger(&units[1].row.file.file_id.to_string())
        .unwrap()
        .unwrap()
        .job_id;
    let retry = wait_for_terminal(&f.store, &retry_id).await;
    assert_eq!(
        retry.status, "succeeded",
        "valid files must remain processable after cancellation: {retry:?}"
    );
}

async fn wait_for_terminal(store: &Arc<Store>, job_id: &str) -> api::store::ProbeJob {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let job = store.as_ref().get_probe_job(job_id).unwrap().unwrap();
            if !job.is_active() {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("job must reach a persisted terminal state")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarted_refresh_still_rejects_unresolved_results() {
    let f = setup("uniform", &[1, 2, 3]);
    let capture = Arc::new(ChangedCapture {
        inner: f.engine.clone(),
        store: f.store.clone(),
        target: f.units[0].row.file.path.to_string_lossy().into_owned(),
        mode: "silent-intro",
    });
    let restarted =
        Arc::new(ProbeManager::with_engines(f.store.clone(), f.engine.clone(), capture).unwrap());
    assert!(restarted.try_start_workers());
    let job = wait_for_terminal(&f.store, "review9-job").await;
    assert_eq!(
        job.status, "failed",
        "recovery must preserve the manual refresh verification requirement"
    );
    assert_eq!(
        f.store
            .as_ref()
            .get_media_marker(f.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "old"
    );
}

fn observed(
    store: &api::store::Store,
    replacements: &[api::store::MarkerResultReplacement],
) -> Vec<api::store::SourceObservation> {
    let ids: Vec<domain::LedgerId> = replacements
        .iter()
        .flat_map(|r| r.chapter_updates.iter().map(|(id, _)| id.parse().unwrap()))
        .collect();
    api::store::observe_sources(&store.source_snapshots(&ids).unwrap())
}
