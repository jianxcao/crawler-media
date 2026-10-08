use super::types::{
    EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel,
    VerificationOutcome, VerifiedInterval,
};
use crate::fingerprint::FingerprintEngine;
use crate::types::CommonSegment;

pub fn verify_template_window(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
) -> VerificationOutcome {
    // Check if target words are empty or silent/constant
    if target.capture.words.is_empty() {
        return VerificationOutcome::NeedsFullWindow {
            reason: "empty_fingerprint".to_string(),
        };
    }
    if super::model::is_constant_or_silent(&target.capture.words) {
        return VerificationOutcome::NeedsFullWindow {
            reason: "constant_or_silent_fingerprint".to_string(),
        };
    }

    // Filter relevant models for this kind
    let matching_models: Vec<&TemplateModel> = templates
        .models
        .iter()
        .filter(|m| m.kind == target.kind && m.references.len() >= 2)
        .collect();

    if matching_models.is_empty() {
        return VerificationOutcome::NeedsFullWindow {
            reason: "no_valid_templates".to_string(),
        };
    }

    // Try matching against each template model
    for model in matching_models {
        if let Some(outcome) = verify_against_model(engine, target, model, templates, policy) {
            return outcome;
        }
    }

    VerificationOutcome::NeedsFullWindow {
        reason: "no_template_matched".to_string(),
    }
}

fn verify_against_model(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    model: &TemplateModel,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
) -> Option<VerificationOutcome> {
    // We need matches against at least two independent reference episodes
    let mut ref_matches: Vec<(String, CommonSegment)> = Vec::new();

    for r in &model.references {
        if r.episode == target.episode {
            continue; // Skip comparing against same episode
        }
        let Some(ref_ev) = templates.references.get(&r.sample_id) else {
            continue;
        };

        let segs = engine.find_common_segments(
            &target.capture.words,
            &ref_ev.capture.words,
            (policy.min_match_duration_ms as f32) / 1000.0,
            (policy.max_match_duration_ms as f32) / 1000.0,
        );
        for s in segs {
            if (s.score as f64) <= policy.max_score {
                ref_matches.push((r.sample_id.clone(), s));
                break;
            }
        }
    }

    if ref_matches.len() < 2 {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: "insufficient_reference_matches".to_string(),
        });
    }

    // Check boundary agreement between references
    let first = &ref_matches[0].1;
    let second = &ref_matches[1].1;

    let target_start1 = target.capture.window.start_ms + (first.start1_sec * 1000.0) as i64;
    let target_end1 = target.capture.window.start_ms + (first.end1_sec * 1000.0) as i64;
    let target_start2 = target.capture.window.start_ms + (second.start1_sec * 1000.0) as i64;
    let target_end2 = target.capture.window.start_ms + (second.end1_sec * 1000.0) as i64;

    let start_delta = (target_start1 - target_start2).abs();
    let end_delta = (target_end1 - target_end2).abs();

    if start_delta > policy.max_reference_boundary_delta_ms
        || end_delta > policy.max_reference_boundary_delta_ms
    {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: "conflicting_references_boundary_delta".to_string(),
        });
    }

    let target_start = target_start1.min(target_start2);
    let target_end = target_end1.max(target_end2);
    let matched_duration = target_end - target_start;

    // Check coverage against model expected duration
    let coverage = matched_duration as f64 / model.expected_duration_ms as f64;
    if coverage < policy.min_reference_coverage {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: format!(
                "coverage_below_threshold: {coverage:.2} < {:.2}",
                policy.min_reference_coverage
            ),
        });
    }

    // Check template edge anchors
    // Both 15s ends of the template must be covered
    let anchor_ms = policy.template_edge_anchor_ms.min(model.expected_duration_ms / 2);
    if matched_duration < anchor_ms * 2 {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: "matched_duration_shorter_than_edge_anchors".to_string(),
        });
    }

    // Guard evidence checks:
    // If matching interval hits the edge of sample without being legitimate media edge (0ms for intro, etc.)
    let left_guard = target_start - target.capture.window.start_ms;
    let right_guard = target.capture.window.end_ms - target_end;

    let is_intro_media_start = target.kind == SegmentKind::Intro && target_start == 0;
    if !is_intro_media_start && left_guard < policy.min_guard_evidence_ms {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: "window_edge_left_boundary_clipped".to_string(),
        });
    }

    if right_guard < policy.min_guard_evidence_ms {
        return Some(VerificationOutcome::NeedsFullWindow {
            reason: "window_edge_right_boundary_clipped".to_string(),
        });
    }

    let avg_score = (first.score + second.score) as f64 / 2.0;

    Some(VerificationOutcome::Verified(VerifiedInterval {
        start_ms: target_start,
        end_ms: target_end,
        supporting_episodes: ref_matches.len(),
        score: avg_score,
        coverage,
    }))
}
