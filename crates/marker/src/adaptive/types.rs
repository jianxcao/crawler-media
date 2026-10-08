use serde::{Deserialize, Serialize};

use crate::fingerprint::capture_types::{CapturedFingerprint, SampleWindow};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Intro,
    Outro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    FullWindow,
    Adaptive,
}

impl Default for SamplingMode {
    fn default() -> Self {
        Self::FullWindow
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SamplingPolicy {
    pub seed_count: usize,
    pub max_seed_count: usize,
    pub max_templates_per_kind: usize,
    pub context_margin_ms: i64,
    pub min_window_saving_ratio: f64,
    pub min_match_duration_ms: i64,
    pub max_match_duration_ms: i64,
    pub max_score: f64,
    pub min_reference_coverage: f64,
    pub max_internal_gap_ms: i64,
    pub max_reference_boundary_delta_ms: i64,
    pub min_guard_evidence_ms: i64,
    pub template_edge_anchor_ms: i64,
    pub max_windows_per_kind: usize,
    pub max_attempts_per_kind: usize,
    pub process_deadline_ms: u64,
    pub full_window_duration_secs: u32,
}

impl Default for SamplingPolicy {
    fn default() -> Self {
        Self {
            seed_count: 3,
            max_seed_count: 5,
            max_templates_per_kind: 3,
            context_margin_ms: 10_000,
            min_window_saving_ratio: 0.15,
            min_match_duration_ms: 15_000,
            max_match_duration_ms: 240_000,
            max_score: 4.0,
            min_reference_coverage: 0.95,
            max_internal_gap_ms: 1_000,
            max_reference_boundary_delta_ms: 1_000,
            min_guard_evidence_ms: 3_000,
            template_edge_anchor_ms: 15_000,
            max_windows_per_kind: 2,
            max_attempts_per_kind: 4,
            process_deadline_ms: 120_000,
            full_window_duration_secs: 180,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpisodeDescriptor {
    pub ledger_id: String,
    pub episode: u32,
    pub source_version: String,
    pub duration_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpisodeEvidence {
    pub sample_id: String,
    pub ledger_id: String,
    pub episode: u32,
    pub source_version: String,
    pub capture_profile_key: String,
    pub kind: SegmentKind,
    pub capture: CapturedFingerprint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateReference {
    pub sample_id: String,
    pub ledger_id: String,
    pub episode: u32,
    pub source_version: String,
    /// Absolute matching interval in this episode [start_ms, end_ms]
    pub match_interval_ms: (i64, i64),
    /// Distance from media end: (duration_ms - end_ms, duration_ms - start_ms)
    pub match_from_end_ms: Option<(i64, i64)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateModel {
    pub model_id: String,
    pub version: u32,
    pub kind: SegmentKind,
    pub references: Vec<TemplateReference>,
    /// Consensus expected duration of this template
    pub expected_duration_ms: i64,
    /// Is this model stable enough for fast verification? (>= 3 supporting episodes)
    pub is_stable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TemplateContext {
    pub models: Vec<TemplateModel>,
    /// Raw evidence mapping by sample_id for fingerprint access
    pub references: std::collections::HashMap<String, EpisodeEvidence>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedInterval {
    pub start_ms: i64,
    pub end_ms: i64,
    pub supporting_episodes: usize,
    pub score: f64,
    pub coverage: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WindowDecision {
    Full {
        window: SampleWindow,
        reason: String,
    },
    Verify {
        window: SampleWindow,
        model_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VerificationOutcome {
    Verified(VerifiedInterval),
    NeedsFullWindow { reason: String },
    NoMatch { reason: String },
    SamplingLimit { reason: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceCostSummary {
    pub median_first_pcm_ms: Option<u64>,
    pub sample_ms_per_wall_ms: Option<f64>,
    pub measured_samples: u32,
    pub recent_fast_attempts: u8,
    pub recent_fallbacks: u8,
}
