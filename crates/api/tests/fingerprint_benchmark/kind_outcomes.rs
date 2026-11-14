use super::*;

struct PartialEngine;
impl marker::FingerprintEngine for PartialEngine {
    fn extract_at(
        &self,
        _: &Path,
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
        let mut candidate = Engine.find_common_segment(first, second, min, max).unwrap();
        if first[0] == 101 {
            candidate.end1_sec = 33.0;
            candidate.end2_sec = 33.0;
            candidate.duration_sec = 3.0;
        }
        Some(candidate)
    }
}
impl marker::FingerprintCaptureEngine for PartialEngine {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let mut capture = Engine.capture_window(request)?;
        capture.words[0] = if request.window.start_ms == 0 { 1 } else { 101 };
        Ok(capture)
    }
}
fn partial_run(locked: bool) -> api::fingerprint_job::benchmark::BenchmarkRun {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &[1, 2, 3]);
    let mut session = session(opts, false);
    session.state = session
        .state
        .with_probe_engines(
            Arc::new(PartialEngine),
            Arc::new(PartialEngine),
            Arc::new(Engine),
        )
        .unwrap();
    for row in session.registered_files() {
        session
            .state
            .store()
            .put_fixture_marker(&store::StoredFileMarker {
                file_id: row.file.file_id,
                version: row.file.version.clone(),
                unit: row.units[0],
                intro_start_ms: locked.then_some(10_000),
                intro_end_ms: locked.then_some(70_000),
                outro_start_ms: Some(250_000),
                outro_end_ms: Some(300_000),
                source: "fingerprint".into(),
                locked,
                updated_at: 1,
            })
            .unwrap();
    }
    let run = session.run().unwrap();
    assert!(run.wallclock_complete, "{run:?}");
    run
}
#[test]
fn partial_benchmark_does_not_report_retained_uncertain_outro_as_fresh_match() {
    let run = partial_run(false);
    for prediction in &run.predictions {
        if prediction.case_id.ends_with("intro") {
            assert_eq!(prediction.state, "matched");
            assert_eq!(prediction.interval_ms, Some([30_000, 90_000]));
        } else {
            assert_eq!(
                prediction.state, "uncertain",
                "retained old Outro must not be called this run's verified result"
            );
            assert_eq!(prediction.interval_ms, None);
            assert!(prediction.reason.contains("verification_unresolved"));
        }
    }
}

#[test]
fn fresh_kind_recognition_does_not_report_retained_manual_interval_as_automatic_match() {
    let run = partial_run(true);
    for prediction in &run.predictions {
        assert_eq!(
            prediction.state, "uncertain",
            "a protected manual interval is not this run's automatic marker"
        );
        assert_eq!(prediction.interval_ms, None);
    }
}
