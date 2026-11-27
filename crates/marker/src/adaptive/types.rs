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
#[derive(Default)]
pub enum SamplingMode {
    #[default]
    FullWindow,
    Adaptive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SamplingPolicy {
    pub boundary: crate::boundary::BoundaryPolicy,
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
            boundary: crate::boundary::BoundaryPolicy::default(),
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
    #[serde(default)]
    pub duration_ms: Option<i64>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptCostObservation {
    pub phase: String,
    pub succeeded: bool,
    pub window_duration_ms: i64,
    pub elapsed_ms: u64,
    pub time_to_first_pcm_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceCostSummary {
    pub median_first_pcm_ms: Option<u64>,
    pub sample_ms_per_wall_ms: Option<f64>,
    pub measured_samples: u32,
    pub recent_fast_attempts: u8,
    pub recent_fallbacks: u8,
}

impl SourceCostSummary {
    pub fn from_observations(observations: &[AttemptCostObservation]) -> Self {
        let mut first_pcms = Vec::new();
        let mut rates = Vec::new();
        let mut measured_samples = 0u32;

        for obs in observations {
            if obs.succeeded && obs.elapsed_ms > 0 && obs.window_duration_ms > 0 {
                measured_samples = measured_samples.saturating_add(1);
                if let Some(first_pcm) = obs.time_to_first_pcm_ms {
                    first_pcms.push(first_pcm);
                }
                let decode_ms = obs
                    .elapsed_ms
                    .saturating_sub(obs.time_to_first_pcm_ms.unwrap_or(0))
                    .max(1);
                let rate = (obs.window_duration_ms as f64 / decode_ms as f64).max(0.01);
                if rate.is_finite() {
                    rates.push(rate);
                }
            }
        }

        first_pcms.sort_unstable();
        rates.sort_by(|a, b| a.total_cmp(b));

        let median_first_pcm_ms = if first_pcms.is_empty() {
            None
        } else {
            Some(first_pcms[first_pcms.len() / 2])
        };
        let sample_ms_per_wall_ms = if rates.is_empty() {
            None
        } else {
            Some(rates[rates.len() / 2])
        };

        let mut fast_outcomes = Vec::new();
        for (idx, obs) in observations.iter().enumerate() {
            let is_fast = obs.phase == "fast_verify"
                || (obs.phase == "scheduled_capture"
                    && obs.window_duration_ms > 0
                    && obs.window_duration_ms < 160_000);
            if !is_fast {
                continue;
            }
            let fell_back = !obs.succeeded
                || observations
                    .get(idx + 1)
                    .is_some_and(|next| next.phase == "fallback_full_window");
            fast_outcomes.push(fell_back);
        }

        let recent_slice = if fast_outcomes.len() > 5 {
            &fast_outcomes[fast_outcomes.len() - 5..]
        } else {
            &fast_outcomes[..]
        };
        let recent_fast_attempts = recent_slice.len() as u8;
        let recent_fallbacks = recent_slice.iter().filter(|&&fb| fb).count() as u8;

        Self {
            median_first_pcm_ms,
            sample_ms_per_wall_ms,
            measured_samples,
            recent_fast_attempts,
            recent_fallbacks,
        }
    }
}
