use super::*;

struct OneWindow {
    head: bool,
}

impl marker::FingerprintCaptureEngine for OneWindow {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if (request.window.start_ms == 0) != self.head {
            return Err(marker::CaptureFailure {
                kind: marker::CaptureFailureKind::Io,
                message: "other window capture forbidden".into(),
                metrics: Default::default(),
            });
        }
        marker::FingerprintCaptureEngine::capture_window(&Engine, request)
    }
}

fn with_one_window(mut evaluation: BenchmarkSession, head: bool) -> BenchmarkSession {
    evaluation.state = evaluation
        .state
        .with_probe_engines(
            Arc::new(Engine),
            Arc::new(OneWindow { head }),
            Arc::new(Engine),
        )
        .unwrap();
    evaluation
}

#[test]
fn natural_retry_reuses_valid_heads_and_rejects_claimed_recapture() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path(), &[1, 2, 3]);
    let failed = with_one_window(session(opts.clone(), false), true)
        .run()
        .unwrap();
    assert!(!failed.wallclock_complete);
    opts.run_id = "retry".into();
    opts.scenario = "retry".into();
    let evaluation = session(opts.clone(), false);
    let heads = evaluation
        .registered_files()
        .iter()
        .map(|row| {
            let id = row.file.file_id.to_string();
            let cache = evaluation
                .state
                .store()
                .get_fingerprint_cache(&id)
                .unwrap()
                .unwrap();
            assert!(!cache.intro.words.is_empty());
            assert!(cache.outro.is_none());
            (id, cache.intro.metrics.captured_job_id)
        })
        .collect::<Vec<_>>();
    drop(evaluation);
    for scenario in ["retry", "incremental"] {
        let mut rejected = opts.clone();
        rejected.scenario = scenario.into();
        rejected.capture_policy = CapturePolicy::Recapture;
        assert!(
            BenchmarkSession::prepare(rejected).is_err(),
            "{scenario} must reject recapture before starting work"
        );
    }
    let evaluation = with_one_window(session(opts, false), false);
    let store = evaluation.state.store().clone();
    let run = evaluation.run().unwrap();
    assert!(
        run.wallclock_complete,
        "{:?} {:?}",
        run.media,
        store.recover_probe_jobs().unwrap()
    );
    assert_eq!(run.capture_policy, CapturePolicy::ReuseValid);
    assert_eq!(run.jobs.len(), 3);
    assert!(run.jobs.iter().all(|job| job["status"] == "succeeded"));
    for (id, previous_job) in heads {
        let cache = store.get_fingerprint_cache(&id).unwrap().unwrap();
        assert_eq!(cache.intro.metrics.captured_job_id, previous_job);
        let tail_job = cache.outro.unwrap().metrics.captured_job_id.unwrap();
        assert!(run.jobs.iter().any(|job| job["id"] == tail_job));
    }
    assert!(
        run.predictions
            .iter()
            .all(|prediction| prediction.state != "error")
    );
}
