use serde::{Deserialize, Serialize};

// Serialization fixtures only: the production runner does not yet export this contract.
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
    pub predictions: Vec<BenchmarkPrediction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkAttempt {
    pub attempt_no: u32,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkPrediction {
    pub case_id: String,
    pub source_hash: String,
    pub state: String,
    pub interval_ms: Option<[u64; 2]>,
    pub reason: String,
    pub run_id: String,
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
        predictions: vec![BenchmarkPrediction {
            case_id: "m1-e1-intro".into(),
            source_hash: "sha256:fixture".into(),
            state: "matched".into(),
            interval_ms: Some([20000, 90000]),
            reason: "verified".into(),
            run_id: "test-run-1".into(),
        }],
    };

    let json_str = serde_json::to_string(&run).expect("serialize");
    let deser: BenchmarkRun = serde_json::from_str(&json_str).expect("deserialize");
    assert_eq!(deser.run_id, "test-run-1");
    assert!(deser.measurement_complete);
    assert_eq!(deser.input_bytes, Some(102400));
    let value: serde_json::Value = serde_json::from_str(&json_str).expect("json value");
    assert!(value.get("accuracy_errors").is_none());
    assert!(value.get("ground_truth").is_none());
    assert_eq!(
        value["predictions"][0]["interval_ms"],
        serde_json::json!([20000, 90000])
    );
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
        predictions: vec![BenchmarkPrediction {
            case_id: "m1-e1-outro".into(),
            source_hash: "sha256:fixture".into(),
            state: "uncertain".into(),
            interval_ms: None,
            reason: "insufficient_evidence".into(),
            run_id: "test-run-2".into(),
        }],
    };

    assert!(!run.measurement_complete);
    assert!(run.input_bytes.is_none());
    let value = serde_json::to_value(&run).expect("serialize");
    assert!(value["input_bytes"].is_null());
    assert!(value["predictions"][0]["interval_ms"].is_null());
}
