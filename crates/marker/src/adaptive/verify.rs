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

    let mut last_failure_reason = "no_template_matched".to_string();

    // Try matching against each template model
    for model in matching_models {
        match verify_against_model(engine, target, model, templates, policy) {
            Ok(verified) => return VerificationOutcome::Verified(verified),
            Err(reason) => {
                last_failure_reason = reason;
            }
        }
    }

    VerificationOutcome::NeedsFullWindow {
        reason: last_failure_reason,
    }
}

fn verify_against_model(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    model: &TemplateModel,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
) -> Result<VerifiedInterval, String> {
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

        let (tpl_start, tpl_end) = r.match_interval_ms;
        for s in segs {
            if (s.score as f64) > policy.max_score {
                continue;
            }

            // Verify coordinates in the reference episode match the template interval
            let ref_start = ref_ev.capture.window.start_ms + (s.start2_sec * 1000.0).round() as i64;
            let ref_end = ref_ev.capture.window.start_ms + (s.end2_sec * 1000.0).round() as i64;

            let start_delta = (ref_start - tpl_start).abs();
            let end_delta = (ref_end - tpl_end).abs();

            // Match must align with the template's reference interval within boundary tolerance
            // and cover both start and end edge anchors
            if start_delta > policy.max_reference_boundary_delta_ms
                || end_delta > policy.max_reference_boundary_delta_ms
            {
                continue;
            }

            ref_matches.push((r.sample_id.clone(), s));
            break;
        }
    }

    if ref_matches.len() < 2 {
        return Err("insufficient_reference_matches".to_string());
    }

    // Find candidate target intervals for all matched references
    #[allow(dead_code)]
    struct MatchedCandidate {
        sample_id: String,
        target_start: i64,
        target_end: i64,
        score: f64,
    }

    let candidates: Vec<MatchedCandidate> = ref_matches
        .into_iter()
        .map(|(sid, s)| {
            let s_start = target.capture.window.start_ms + (s.start1_sec * 1000.0).round() as i64;
            let s_end = target.capture.window.start_ms + (s.end1_sec * 1000.0).round() as i64;
            MatchedCandidate {
                sample_id: sid,
                target_start: s_start,
                target_end: s_end,
                score: s.score,
            }
        })
        .collect();

    // Group candidates into consensus clusters where all members agree within max_reference_boundary_delta_ms.
    // Order-independent: find the cluster with the largest support (and tie-break by lowest avg score).
    let mut best_cluster: Vec<&MatchedCandidate> = Vec::new();
    for i in 0..candidates.len() {
        let mut cluster = vec![&candidates[i]];
        for j in 0..candidates.len() {
            if i == j {
                continue;
            }
            // Candidate j joins the cluster if it agrees with all existing members of the cluster
            let matches_all = cluster.iter().all(|c| {
                (c.target_start - candidates[j].target_start).abs() <= policy.max_reference_boundary_delta_ms
                    && (c.target_end - candidates[j].target_end).abs() <= policy.max_reference_boundary_delta_ms
            });
            if matches_all {
                cluster.push(&candidates[j]);
            }
        }
        if cluster.len() > best_cluster.len() {
            best_cluster = cluster;
        } else if cluster.len() == best_cluster.len() && !cluster.is_empty() {
            let avg1: f64 = cluster.iter().map(|c| c.score).sum::<f64>() / cluster.len() as f64;
            let avg2: f64 = best_cluster.iter().map(|c| c.score).sum::<f64>() / best_cluster.len() as f64;
            if avg1 < avg2 {
                best_cluster = cluster;
            }
        }
    }

    if best_cluster.len() < 2 {
        return Err("insufficient_consensus_reference_matches".to_string());
    }

    let target_start = best_cluster.iter().map(|c| c.target_start).min().unwrap();
    let target_end = best_cluster.iter().map(|c| c.target_end).max().unwrap();
    let matched_duration = target_end - target_start;

    // Check coverage against model expected duration
    let coverage = matched_duration as f64 / model.expected_duration_ms as f64;
    if coverage < policy.min_reference_coverage {
        return Err(format!(
            "coverage_below_threshold: {coverage:.2} < {:.2}",
            policy.min_reference_coverage
        ));
    }

    // Check template edge anchors
    let anchor_ms = policy.template_edge_anchor_ms.min(model.expected_duration_ms / 2);
    if matched_duration < anchor_ms * 2 {
        return Err("matched_duration_shorter_than_edge_anchors".to_string());
    }

    // Guard evidence checks:
    // Check actual decoded PCM boundaries instead of requested window end to prevent truncated audio attacks
    let actual_decoded_end_ms = match target.capture.pcm_duration_ms {
        Some(pcm_ms) => target.capture.window.start_ms + pcm_ms,
        None => target.capture.window.end_ms,
    };
    let left_guard = target_start - target.capture.window.start_ms;
    let right_guard = actual_decoded_end_ms - target_end;

    let is_intro_media_start = target.kind == SegmentKind::Intro && target_start == 0;
    if !is_intro_media_start && left_guard < policy.min_guard_evidence_ms {
        return Err("window_edge_left_boundary_clipped".to_string());
    }

    if right_guard < policy.min_guard_evidence_ms {
        return Err("window_edge_right_boundary_clipped".to_string());
    }

    let avg_score = best_cluster.iter().map(|c| c.score).sum::<f64>() / best_cluster.len() as f64;

    Ok(VerifiedInterval {
        start_ms: target_start,
        end_ms: target_end,
        supporting_episodes: best_cluster.len(),
        score: avg_score,
        coverage,
    })
}
