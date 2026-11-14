//! Production decisions retain abstention independently of candidate extraction.
use std::collections::{HashMap, HashSet};

use crate::boundary::evidence::{complete_pcm, review_candidate};
use crate::boundary::{BoundaryDecision, Interval, normalize_capture_interval};
use crate::{EpisodeEvidence, FingerprintEngine, SamplingPolicy, SegmentKind};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum FingerprintDecision {
    Verified(Interval),
    NoMatch,
    Uncertain(String),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EpisodeFingerprintDecision {
    pub sample_id: String,
    pub ledger_id: String,
    pub episode: u32,
    pub kind: SegmentKind,
    pub window: crate::SampleWindow,
    pub decision: FingerprintDecision,
}

/// Decide every supplied episode/kind from actual capture evidence. Candidate-only
/// APIs intentionally omit abstentions; automatic publication must use this seam.
/// NoMatch proves absence only in the returned window; callers must check any
/// previous interval is covered before clearing it.
pub fn verify_season_fingerprints_with(
    engine: &dyn FingerprintEngine,
    evidences: &[EpisodeEvidence],
    chapters: &HashMap<String, Vec<Interval>>,
    policy: &SamplingPolicy,
) -> Vec<EpisodeFingerprintDecision> {
    let mut decisions = Vec::with_capacity(evidences.len());
    for kind in [SegmentKind::Intro, SegmentKind::Outro] {
        let valid = evidences
            .iter()
            .filter(|e| e.kind == kind && usable(e))
            .collect::<Vec<_>>();
        let episodes = valid.iter().map(|e| e.episode).collect::<Vec<_>>();
        let consensus = super::consensus::find_season_consensus(
            &episodes,
            policy.min_match_duration_ms as f32 / 1000.0,
            policy.max_match_duration_ms as f32 / 1000.0,
            |first, second| {
                engine.compare_evidence(
                    valid[first],
                    valid[second],
                    policy.min_match_duration_ms as f32 / 1000.0,
                    policy.max_match_duration_ms as f32 / 1000.0,
                )
            },
        );
        for target in evidences.iter().filter(|e| e.kind == kind) {
            let decision = match valid.iter().position(|e| std::ptr::eq(*e, target)) {
                None => uncertain("incomplete_or_unusable_fingerprint"),
                Some(index) => {
                    decide_target(engine, target, index, &valid, &consensus, chapters, policy)
                }
            };
            decisions.push(EpisodeFingerprintDecision {
                sample_id: target.sample_id.clone(),
                ledger_id: target.ledger_id.clone(),
                episode: target.episode,
                kind,
                window: target.capture.window.clone(),
                decision,
            });
        }
    }
    decisions
}

fn usable(evidence: &EpisodeEvidence) -> bool {
    crate::boundary::evidence::candidate_pcm_coverage(&evidence.capture).is_some()
        && !crate::adaptive::model::is_constant_or_silent(&evidence.capture.words)
}

fn uncertain(reason: &str) -> FingerprintDecision {
    FingerprintDecision::Uncertain(reason.into())
}

fn decide_target(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    index: usize,
    valid: &[&EpisodeEvidence],
    consensus: &super::consensus::SeasonConsensus,
    chapters: &HashMap<String, Vec<Interval>>,
    policy: &SamplingPolicy,
) -> FingerprintDecision {
    if let Some(range) = consensus.ranges.iter().find(|r| r.episode_index == index) {
        let candidate = normalize_capture_interval(
            &target.capture,
            Interval {
                start_ms: (range.start_secs * 1000.0).round() as i64,
                end_ms: (range.end_secs * 1000.0).round() as i64,
            },
        );
        let Ok(candidate) = candidate else {
            return uncertain("candidate_outside_capture");
        };
        return match review_candidate(
            engine,
            target,
            valid,
            candidate,
            chapters
                .get(&target.ledger_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            policy,
        ) {
            BoundaryDecision::Verified(interval) => FingerprintDecision::Verified(interval),
            BoundaryDecision::Uncertain(reason) => uncertain(reason),
        };
    }
    if !consensus.uncertain_reasons[index].is_empty() {
        let mut reasons = consensus.uncertain_reasons[index]
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        reasons.sort();
        return uncertain(&format!(
            "pair_comparison_unresolved: {}",
            reasons.join("; ")
        ));
    }
    if consensus.raw_candidates[index] {
        return uncertain("candidate_rejected_ambiguous_or_insufficient_support");
    }
    let independent_empty = consensus.empty_references[index]
        .iter()
        .map(|i| valid[*i])
        .filter(|reference| complete_full_window(reference, policy))
        .map(|reference| reference.episode)
        .collect::<HashSet<_>>();
    if complete_full_window(target, policy) && independent_empty.len() >= 2 {
        FingerprintDecision::NoMatch
    } else {
        uncertain("insufficient_complete_reference_comparisons")
    }
}

fn complete_full_window(e: &EpisodeEvidence, policy: &SamplingPolicy) -> bool {
    if !complete_pcm(&e.capture) {
        return false;
    }
    let full_ms = i64::from(policy.full_window_duration_secs) * 1000;
    match e.kind {
        SegmentKind::Intro => {
            e.capture.window.start_ms == 0
                && e.capture.window.end_ms
                    >= e.duration_ms
                        .filter(|d| *d > 0)
                        .unwrap_or(full_ms)
                        .min(full_ms)
        }
        SegmentKind::Outro => e.duration_ms.is_some_and(|duration| {
            duration > 0
                && e.capture.window.end_ms == duration
                && e.capture.window.start_ms <= (duration - full_ms).max(0)
        }),
    }
}
