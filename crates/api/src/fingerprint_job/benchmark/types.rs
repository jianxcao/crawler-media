use crate::fingerprint_job::CapturePolicy;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Completed,
    Failed,
    Incomplete,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCase {
    pub case_id: String,
    pub media_group: String,
    pub split: String,
    pub source_hash: String,
    pub duration_ms: i64,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkManifest {
    pub corpus_version: String,
    pub evidence_kind: String,
    pub cases: Vec<BenchmarkCase>,
    pub acceptance_policy: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceUnit {
    pub season: i32,
    pub episode: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSource {
    pub case_id: String,
    pub path: PathBuf,
    pub audio_stream_index: u32,
    pub unit: SourceUnit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkPrediction {
    pub case_id: String,
    pub source_hash: String,
    pub state: String,
    pub interval_ms: Option<[i64; 2]>,
    pub reason: String,
    pub run_id: String,
}

/// Durations are sums of observed operation spans, not a partition of job wallclock.
/// Compare/refine are disjoint CPU observations. Uninstrumented work stays null.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkStages {
    pub queue_wait: Option<u64>,
    pub remote_gate_wait: Option<u64>,
    pub capture: Option<u64>,
    pub compare: Option<u64>,
    pub refine: Option<u64>,
    pub frame: Option<u64>,
    pub publish: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkAttempt {
    pub operation: String,
    pub success: bool,
    pub duration_ms: Option<u64>,
    pub error_kind: Option<String>,
    pub metrics: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildProvenance {
    pub profile: String,
    pub opt_level: String,
    pub debug: String,
    pub target: String,
    pub rustc: String,
    pub encoded_rustflags: String,
    pub dependency_lock_sha256: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub effective_settings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkRun {
    pub schema_version: u32,
    pub run_id: String,
    pub pair_id: String,
    pub variant: String,
    pub scenario: String,
    pub corpus_version: String,
    pub config_hash: String,
    pub source_hashes: Vec<String>,
    pub commit: String,
    pub mode: String,
    pub capture_policy: CapturePolicy,
    pub build: BuildProvenance,
    pub total_elapsed_ms: u64,
    pub wallclock_complete: bool,
    pub terminal_status: TerminalStatus,
    pub stage_totals: BenchmarkStages,
    pub input_bytes: Option<u64>,
    pub measurement_complete: bool,
    pub raw_pairs_computed: Option<u64>,
    pub raw_pairs_reused: Option<u64>,
    pub attempts: Vec<BenchmarkAttempt>,
    pub predictions: Vec<BenchmarkPrediction>,
    pub jobs: Vec<serde_json::Value>,
    pub frames: Vec<serde_json::Value>,
    pub source_verification: Vec<serde_json::Value>,
    pub limitations: Vec<String>,
    pub media: Vec<MediaMeasurement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaMeasurement {
    pub media_group: String,
    pub media_id: domain::MediaId,
    pub file_count: usize,
    pub independent_episode_count: usize,
    pub elapsed_ms: u64,
    pub wallclock_complete: bool,
    pub terminal_status: TerminalStatus,
    pub job_ids: Vec<String>,
    pub frame_targets: usize,
    pub ready_frames: usize,
}

#[derive(Clone, Debug)]
pub struct BenchmarkOptions {
    pub manifest: PathBuf,
    pub sources: PathBuf,
    pub mode: String,
    pub capture_policy: CapturePolicy,
    pub data_dir: PathBuf,
    pub output: PathBuf,
    pub run_id: String,
    pub pair_id: String,
    pub variant: String,
    pub scenario: String,
    pub allow_remote: bool,
    pub deadline_ms: u64,
}
