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

    let full_window_ms = (policy.full_window_duration_secs as i64) * 1000;
    let is_full_window = target.capture.window.duration_ms() >= full_window_ms
        || target
            .duration_ms
            .map(|dur| dur > 0 && target.capture.window.duration_ms() >= dur)
            .unwrap_or(false);

    let is_complete_pcm = match target.capture.pcm_duration_ms {
        Some(pcm_ms) => pcm_ms + 1000 >= target.capture.window.duration_ms(),
        None => false,
    };

    // If no raw candidate matches at all on a complete PCM full window, return NoMatch
    if last_failure_reason == "zero_engine_segments" && is_full_window && is_complete_pcm {
        VerificationOutcome::NoMatch {
            reason: last_failure_reason,
        }
    } else {
        VerificationOutcome::NeedsFullWindow {
            reason: last_failure_reason,
        }
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
    let mut raw_segments_found = 0;

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

        raw_segments_found += segs.len();

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

    if raw_segments_found == 0 {
        return Err("zero_engine_segments".to_string());
    }

    let is_target_model_ref = model.references.iter().any(|r| {
        r.ledger_id == target.ledger_id
            && r.sample_id == target.sample_id
            && r.source_version == target.source_version
    });
    let min_required_matches = if is_target_model_ref { 1 } else { 2 };

    if ref_matches.is_empty() {
        return Err("no_aligned_reference_matches".to_string());
    }

    if ref_matches.len() < min_required_matches {
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

    // Build all maximal consensus clusters.
    // Rather than greedy sequential inclusion which depends on candidate ordering,
    // examine all compatible cliques. For candidates.len() <= 12, we can enumerate
    // the power set of candidate indices to guarantee finding the optimal consensus subset.
    let mut clusters: Vec<Vec<&MatchedCandidate>> = Vec::new();
    let n = candidates.len();
    if n <= 12 {
        for mask in 1..(1usize << n) {
            let subset: Vec<&MatchedCandidate> = (0..n)
                .filter(|k| (mask & (1 << k)) != 0)
                .map(|k| &candidates[k])
                .collect();
            let min_start = subset.iter().map(|c| c.target_start).min().unwrap();
            let max_start = subset.iter().map(|c| c.target_start).max().unwrap();
            let min_end = subset.iter().map(|c| c.target_end).min().unwrap();
            let max_end = subset.iter().map(|c| c.target_end).max().unwrap();
            if max_start - min_start <= policy.max_reference_boundary_delta_ms
                && max_end - min_end <= policy.max_reference_boundary_delta_ms
            {
                let mut sorted_sub = subset;
                sorted_sub.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
                clusters.push(sorted_sub);
            }
        }
    } else {
        for i in 0..candidates.len() {
            let mut cluster = vec![&candidates[i]];
            for j in 0..candidates.len() {
                if i == j {
                    continue;
                }
                let matches_all = cluster.iter().all(|c| {
                    (c.target_start - candidates[j].target_start).abs() <= policy.max_reference_boundary_delta_ms
                        && (c.target_end - candidates[j].target_end).abs() <= policy.max_reference_boundary_delta_ms
                });
                if matches_all {
                    cluster.push(&candidates[j]);
                }
            }
            cluster.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
            clusters.push(cluster);
        }
    }

    if clusters.is_empty() {
        return Err("insufficient_consensus_reference_matches".to_string());
    }

    // Sort clusters: first by size (descending), then by average score (ascending)
    clusters.sort_by(|c1, c2| {
        let len_cmp = c2.len().cmp(&c1.len());
        if len_cmp != std::cmp::Ordering::Equal {
            return len_cmp;
        }
        let avg1: f64 = c1.iter().map(|c| c.score).sum::<f64>() / c1.len() as f64;
        let avg2: f64 = c2.iter().map(|c| c.score).sum::<f64>() / c2.len() as f64;
        avg1.partial_cmp(&avg2).unwrap_or(std::cmp::Ordering::Equal)
    });

    let best_cluster = &clusters[0];
    if best_cluster.len() < min_required_matches {
        return Err("insufficient_consensus_reference_matches".to_string());
    }

    // Now check if there is an ambiguous tie with another cluster having equal size and equal score
    let best_avg: f64 = best_cluster.iter().map(|c| c.score).sum::<f64>() / best_cluster.len() as f64;
    for other in &clusters[1..] {
        if other.len() != best_cluster.len() {
            break;
        }
        let other_avg: f64 = other.iter().map(|c| c.score).sum::<f64>() / other.len() as f64;
        if (best_avg - other_avg).abs() <= 1e-6 {
            let start1 = best_cluster.iter().map(|c| c.target_start).min().unwrap();
            let end1 = best_cluster.iter().map(|c| c.target_end).max().unwrap();
            let start2 = other.iter().map(|c| c.target_start).min().unwrap();
            let end2 = other.iter().map(|c| c.target_end).max().unwrap();
            if (start1 - start2).abs() > policy.max_reference_boundary_delta_ms
                || (end1 - end2).abs() > policy.max_reference_boundary_delta_ms
            {
                return Err("ambiguous_consensus_clusters_tie".to_string());
            }
        }
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
    // Check actual decoded PCM boundaries instead of requested window end to prevent truncated audio attacks.
    // If PCM coverage is unknown (None), guard evidence cannot be reliably verified.
    let actual_decoded_end_ms = match target.capture.pcm_duration_ms {
        Some(pcm_ms) => target.capture.window.start_ms + pcm_ms,
        None => return Err("pcm_coverage_unknown".to_string()),
    };
    let left_guard = target_start - target.capture.window.start_ms;
    let right_guard = actual_decoded_end_ms - target_end;

    let is_intro_media_start = target.kind == SegmentKind::Intro && target_start == 0;
    if !is_intro_media_start && left_guard < policy.min_guard_evidence_ms {
        return Err("window_edge_left_boundary_clipped".to_string());
    }

    let is_outro_media_end = target.kind == SegmentKind::Outro
        && target
            .duration_ms
            .map(|dur| (dur - target_end).abs() <= policy.max_reference_boundary_delta_ms)
            .unwrap_or(false);

    if !is_outro_media_end && right_guard < policy.min_guard_evidence_ms {
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
