use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkRun {
    pub baseline_commit: String,
    pub implementation_commit: String,
    pub run_id: String,
    pub sampling_mode: String,
    pub capture_policy: String,
    pub source_versions: Vec<String>,
    pub queue_wait_ms: u64,
    pub priority_wait_ms: u64,
    pub total_elapsed_ms: u64,
    pub stage_totals: Vec<(String, u64)>,
    pub attempts: Vec<BenchmarkAttempt>,
    pub input_bytes: Option<u64>,
    pub measurement_complete: bool,
    pub episode_intervals: Vec<BenchmarkInterval>,
    pub ground_truth: Vec<BenchmarkInterval>,
    pub accuracy_errors: Vec<AccuracyError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkAttempt {
    pub attempt_no: u32,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkInterval {
    pub episode: u32,
    pub marker_type: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccuracyError {
    pub episode: u32,
    pub marker_type: String,
    pub boundary_error_ms: u64,
}

#[test]
fn benchmark_run_serialization_and_measurement_completeness() {
    let run = BenchmarkRun {
        baseline_commit: "6fc3f9c".to_string(),
        implementation_commit: "072d87d".to_string(),
        run_id: "test-run-1".to_string(),
        sampling_mode: "adaptive".to_string(),
        capture_policy: "recapture".to_string(),
        source_versions: vec!["src_v1".to_string()],
        queue_wait_ms: 10,
        priority_wait_ms: 0,
        total_elapsed_ms: 1250,
        stage_totals: vec![("capture".to_string(), 1200)],
        attempts: vec![BenchmarkAttempt {
            attempt_no: 1,
            success: true,
            duration_ms: 1200,
        }],
        input_bytes: Some(102400),
        measurement_complete: true,
        episode_intervals: vec![],
        ground_truth: vec![],
        accuracy_errors: vec![],
    };

    let json_str = serde_json::to_string(&run).expect("serialize");
    let deser: BenchmarkRun = serde_json::from_str(&json_str).expect("deserialize");
    assert_eq!(deser.run_id, "test-run-1");
    assert!(deser.measurement_complete);
    assert_eq!(deser.input_bytes, Some(102400));
}

#[test]
fn unknown_network_bytes_preserves_incomplete_metrics() {
    let run = BenchmarkRun {
        baseline_commit: "6fc3f9c".to_string(),
        implementation_commit: "072d87d".to_string(),
        run_id: "test-run-2".to_string(),
        sampling_mode: "full_window".to_string(),
        capture_policy: "reuse_valid".to_string(),
        source_versions: vec!["src_v1".to_string()],
        queue_wait_ms: 0,
        priority_wait_ms: 0,
        total_elapsed_ms: 50,
        stage_totals: vec![],
        attempts: vec![],
        input_bytes: None,
        measurement_complete: false,
        episode_intervals: vec![],
        ground_truth: vec![],
        accuracy_errors: vec![],
    };

    assert!(!run.measurement_complete);
    assert!(run.input_bytes.is_none());
}
