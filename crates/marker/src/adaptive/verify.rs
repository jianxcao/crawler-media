use super::types::{
    EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateContext, TemplateModel,
    VerificationOutcome, VerifiedInterval,
};
use crate::fingerprint::FingerprintEngine;
use crate::types::CommonSegment;

/// Whether one template's failure is a *completed* comparison that found no candidate at all.
///
/// Only an empty engine result counts as a confirmed no-match. Comparisons that found segments
/// but could not align, score or support them stay inconclusive: they must fall back to the
/// full window instead of clearing existing markers, and their outcome must survive the
/// aggregation below even when another template reports an empty engine result.
fn template_confirms_no_match(reason: &str) -> bool {
    reason == "zero_engine_segments"
}

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

    // Try matching against each template model, keeping every model's outcome so that one
    // template without candidates cannot overwrite another template's inconclusive result.
    let mut failure_reasons: Vec<String> = Vec::new();
    let mut all_templates_confirm_no_match = true;

    for model in matching_models {
        match verify_against_model(engine, target, model, templates, policy) {
            Ok(verified) => return VerificationOutcome::Verified(verified),
            Err(reason) => {
                if !template_confirms_no_match(&reason) {
                    all_templates_confirm_no_match = false;
                }
                failure_reasons.push(reason);
            }
        }
    }

    let reason = aggregate_failure_reasons(all_templates_confirm_no_match, &failure_reasons);

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

    // "Successfully no match" may only be concluded when every applicable template completed
    // its comparison without candidates on a complete PCM full window.
    if all_templates_confirm_no_match && is_full_window && is_complete_pcm {
        VerificationOutcome::NoMatch { reason }
    } else {
        VerificationOutcome::NeedsFullWindow { reason }
    }
}

/// Combine per-template failure reasons without discarding any template's outcome.
fn aggregate_failure_reasons(all_confirm_no_match: bool, reasons: &[String]) -> String {
    let Some(first) = reasons.first() else {
        return "no_template_matched".to_string();
    };

    if all_confirm_no_match {
        // Every template confirmed no match, so every reason is the same empty-engine result.
        return first.clone();
    }

    let mut unique: Vec<&str> = Vec::new();
    for reason in reasons {
        if !unique.contains(&reason.as_str()) {
            unique.push(reason.as_str());
        }
    }
    format!("template_results_disagree: {}", unique.join("; "))
}

fn verify_against_model(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    model: &TemplateModel,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
) -> Result<VerifiedInterval, String> {
    // We need matches against at least two independent reference episodes
    let (raw_segments_found, ref_matches) =
        collect_reference_matches(engine, target, model, templates, policy);

    if raw_segments_found == 0 {
        return Err("zero_engine_segments".to_string());
    }

    let min_required_matches = min_required_matches(model, target);

    let independent_matched_episodes = count_unique_episodes(&ref_matches);
    if independent_matched_episodes < min_required_matches {
        return Err("insufficient_reference_matches".to_string());
    }

    // Find candidate target intervals for all matched references
    let candidates = build_candidates(target, ref_matches);

    // Build all maximal consensus clusters. Rather than greedy sequential inclusion which
    // depends on candidate ordering, examine all compatible cliques. For small candidate
    // counts we can enumerate the power set to guarantee finding the optimal consensus subset.
    let mut clusters = build_clusters(&candidates, policy);

    if clusters.is_empty() {
        return Err("insufficient_consensus_reference_matches".to_string());
    }

    sort_clusters_by_consensus(&mut clusters);

    let best_cluster = &clusters[0];
    let best_unique_episodes = count_cluster_episodes(best_cluster);
    if best_unique_episodes < min_required_matches {
        return Err("insufficient_consensus_reference_matches".to_string());
    }

    ensure_cluster_consensus_is_unambiguous(&clusters, best_cluster, policy)?;

    let target_start = best_cluster.iter().map(|c| c.target_start).min().unwrap();
    let target_end = best_cluster.iter().map(|c| c.target_end).max().unwrap();
    let matched_duration = target_end - target_start;

    // Check coverage against model expected duration
    let coverage = matched_duration as f64 / model.expected_duration_ms as f64;

    validate_interval_evidence(target, model, policy, target_start, target_end, coverage)?;

    Ok(VerifiedInterval {
        start_ms: target_start,
        end_ms: target_end,
        supporting_episodes: best_unique_episodes,
        score: average_score(best_cluster),
        coverage,
    })
}

fn count_unique_episodes(ref_matches: &[(String, u32, CommonSegment)]) -> usize {
    ref_matches
        .iter()
        .map(|(_, ep, _)| *ep)
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn count_cluster_episodes(cluster: &[&MatchedCandidate]) -> usize {
    cluster
        .iter()
        .map(|c| c.episode)
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Compare the target against every reference of one model and keep only the segments whose
/// coordinates in the reference episode align with this model's template interval.
fn collect_reference_matches(
    engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence,
    model: &TemplateModel,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
) -> (usize, Vec<(String, u32, CommonSegment)>) {
    let mut ref_matches: Vec<(String, u32, CommonSegment)> = Vec::new();
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

            ref_matches.push((r.sample_id.clone(), r.episode, s));
            break;
        }
    }

    (raw_segments_found, ref_matches)
}

fn min_required_matches(model: &TemplateModel, target: &EpisodeEvidence) -> usize {
    let is_target_model_ref = model.references.iter().any(|r| {
        r.ledger_id == target.ledger_id
            && r.sample_id == target.sample_id
            && r.source_version == target.source_version
    });
    if is_target_model_ref { 1 } else { 2 }
}

struct MatchedCandidate {
    sample_id: String,
    episode: u32,
    target_start: i64,
    target_end: i64,
    score: f64,
}

fn build_candidates(
    target: &EpisodeEvidence,
    ref_matches: Vec<(String, u32, CommonSegment)>,
) -> Vec<MatchedCandidate> {
    // Repeated file versions cannot affect cluster enumeration or its size cutoff.
    let mut seen = std::collections::BTreeSet::new();
    ref_matches
        .into_iter()
        .filter_map(|(sid, ep, s)| {
            let s_start = target.capture.window.start_ms + (s.start1_sec * 1000.0).round() as i64;
            let s_end = target.capture.window.start_ms + (s.end1_sec * 1000.0).round() as i64;
            seen.insert((ep, s_start, s_end, s.score.to_bits()))
                .then_some(MatchedCandidate {
                    sample_id: sid,
                    episode: ep,
                    target_start: s_start,
                    target_end: s_end,
                    score: s.score,
                })
        })
        .collect()
}

fn build_clusters<'a>(
    candidates: &'a [MatchedCandidate],
    policy: &SamplingPolicy,
) -> Vec<Vec<&'a MatchedCandidate>> {
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
                    (c.target_start - candidates[j].target_start).abs()
                        <= policy.max_reference_boundary_delta_ms
                        && (c.target_end - candidates[j].target_end).abs()
                            <= policy.max_reference_boundary_delta_ms
                });
                if matches_all {
                    cluster.push(&candidates[j]);
                }
            }
            cluster.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
            clusters.push(cluster);
        }
    }

    clusters
}

/// Sort clusters: first by unique supporting episodes count (descending), then by average score (ascending).
fn sort_clusters_by_consensus(clusters: &mut [Vec<&MatchedCandidate>]) {
    clusters.sort_by(|c1, c2| {
        let ep1 = count_cluster_episodes(c1);
        let ep2 = count_cluster_episodes(c2);
        let len_cmp = ep2.cmp(&ep1);
        if len_cmp != std::cmp::Ordering::Equal {
            return len_cmp;
        }
        average_score(c1)
            .partial_cmp(&average_score(c2))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Reject an ambiguous tie between equally sized, equally scored clusters that disagree on
/// the matched interval.
fn ensure_cluster_consensus_is_unambiguous(
    clusters: &[Vec<&MatchedCandidate>],
    best_cluster: &[&MatchedCandidate],
    policy: &SamplingPolicy,
) -> Result<(), String> {
    let best_avg = average_score(best_cluster);
    let best_episodes = count_cluster_episodes(best_cluster);
    for other in &clusters[1..] {
        if count_cluster_episodes(other) != best_episodes {
            break;
        }
        if (best_avg - average_score(other)).abs() > 1e-6 {
            continue;
        }
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
    Ok(())
}

fn average_score(cluster: &[&MatchedCandidate]) -> f64 {
    let mut scores = std::collections::BTreeMap::<u32, f64>::new();
    for candidate in cluster {
        scores
            .entry(candidate.episode)
            .and_modify(|score| *score = score.min(candidate.score))
            .or_insert(candidate.score);
    }
    scores.values().sum::<f64>() / scores.len() as f64
}

/// Check coverage, template edge anchors and guard evidence around the matched interval.
///
/// Guard evidence uses the actual decoded PCM boundaries instead of the requested window end
/// to prevent truncated audio attacks. If PCM coverage is unknown (None), guard evidence
/// cannot be reliably verified.
fn validate_interval_evidence(
    target: &EpisodeEvidence,
    model: &TemplateModel,
    policy: &SamplingPolicy,
    target_start: i64,
    target_end: i64,
    coverage: f64,
) -> Result<(), String> {
    if coverage < policy.min_reference_coverage {
        return Err(format!(
            "coverage_below_threshold: {coverage:.2} < {:.2}",
            policy.min_reference_coverage
        ));
    }

    let matched_duration = target_end - target_start;

    // Check template edge anchors
    let anchor_ms = policy
        .template_edge_anchor_ms
        .min(model.expected_duration_ms / 2);
    if matched_duration < anchor_ms * 2 {
        return Err("matched_duration_shorter_than_edge_anchors".to_string());
    }

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

    Ok(())
}
