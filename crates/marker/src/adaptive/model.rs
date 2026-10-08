use std::collections::{HashMap, HashSet};

use super::types::{
    EpisodeDescriptor, EpisodeEvidence, SamplingPolicy, SegmentKind, TemplateModel,
    TemplateReference,
};
use crate::fingerprint::FingerprintEngine;
use crate::types::CommonSegment;

pub fn select_seed_episodes(
    episodes: &[EpisodeDescriptor],
    policy: &SamplingPolicy,
) -> Vec<String> {
    if episodes.is_empty() {
        return Vec::new();
    }

    // Sort by (episode, ledger_id)
    let mut sorted = episodes.to_vec();
    sorted.sort_by(|a, b| a.episode.cmp(&b.episode).then_with(|| a.ledger_id.cmp(&b.ledger_id)));

    // De-duplicate by episode number: take first ledger_id for each episode
    let mut unique_episodes: Vec<&EpisodeDescriptor> = Vec::new();
    let mut seen_ep_nums = HashSet::new();
    for ep in &sorted {
        if seen_ep_nums.insert(ep.episode) {
            unique_episodes.push(ep);
        }
    }

    let n = unique_episodes.len();
    if n == 0 {
        return Vec::new();
    }

    let target_count = policy.seed_count.min(n);
    let mut chosen_indices = Vec::new();

    if n >= 6 {
        // N >= 6: index 1, (N-1)/2, N-2
        let idx1 = 1;
        let idx2 = (n - 1) / 2;
        let idx3 = n - 2;

        for idx in [idx1, idx2, idx3] {
            if !chosen_indices.contains(&idx) && idx < n {
                chosen_indices.push(idx);
            }
        }
        // Fill up to target_count if needed
        for i in 0..n {
            if chosen_indices.len() >= target_count {
                break;
            }
            if !chosen_indices.contains(&i) {
                chosen_indices.push(i);
            }
        }
    } else {
        // 2 <= N < 6: take up to target_count sequentially
        for i in 0..target_count {
            chosen_indices.push(i);
        }
    }

    chosen_indices
        .into_iter()
        .map(|idx| unique_episodes[idx].ledger_id.clone())
        .collect()
}

pub fn is_constant_or_silent(words: &[u32]) -> bool {
    if words.is_empty() {
        return true;
    }
    let first = words[0];
    let all_same = words.iter().all(|&w| w == first);
    if all_same {
        return true;
    }
    // Check silence (majority 0s)
    let zeros = words.iter().filter(|&&w| w == 0).count();
    if zeros as f64 / words.len() as f64 > 0.95 {
        return true;
    }
    false
}

pub fn build_season_models(
    engine: &dyn FingerprintEngine,
    evidence: &[EpisodeEvidence],
    policy: &SamplingPolicy,
) -> Vec<TemplateModel> {
    if evidence.is_empty() {
        return Vec::new();
    }

    // Filter evidence by kind; group evidence by kind
    let mut models = Vec::new();
    for &kind in &[SegmentKind::Intro, SegmentKind::Outro] {
        let kind_evidence: Vec<&EpisodeEvidence> = evidence
            .iter()
            .filter(|e| {
                if e.kind != kind || is_constant_or_silent(&e.capture.words) {
                    return false;
                }
                // Disallow truncated audio captures that did not decode sufficient audio for the requested window
                if let Some(pcm_ms) = e.capture.pcm_duration_ms {
                    if pcm_ms < e.capture.window.duration_ms().min(policy.min_match_duration_ms + policy.min_guard_evidence_ms) {
                        return false;
                    }
                }
                true
            })
            .collect();

        if kind_evidence.len() < 2 {
            continue;
        }

        let kind_models = build_models_for_kind(engine, &kind_evidence, kind, policy);
        models.extend(kind_models);
    }

    models
}

struct PairCandidate {
    first_idx: usize,
    second_idx: usize,
    segment: CommonSegment,
}

fn build_models_for_kind(
    engine: &dyn FingerprintEngine,
    evidence: &[&EpisodeEvidence],
    kind: SegmentKind,
    policy: &SamplingPolicy,
) -> Vec<TemplateModel> {
    let mut matches = Vec::new();
    for i in 0..evidence.len() {
        for j in i + 1..evidence.len() {
            // Distinct episodes required for templates
            if evidence[i].episode == evidence[j].episode {
                continue;
            }

            let segments = engine.find_common_segments(
                &evidence[i].capture.words,
                &evidence[j].capture.words,
                (policy.min_match_duration_ms as f32) / 1000.0,
                (policy.max_match_duration_ms as f32) / 1000.0,
            );

            for seg in segments {
                let duration_ms = (seg.duration_sec * 1000.0) as i64;
                if duration_ms >= policy.min_match_duration_ms
                    && duration_ms <= policy.max_match_duration_ms
                    && (seg.score as f64) <= policy.max_score
                {
                    matches.push(PairCandidate {
                        first_idx: i,
                        second_idx: j,
                        segment: seg,
                    });
                }
            }
        }
    }

    if matches.is_empty() {
        return Vec::new();
    }

    // Cluster matching pairs
    let clusters = cluster_matches(&matches, evidence);

    let mut template_models = Vec::new();
    for (idx, cluster) in clusters.into_iter().enumerate() {
        // Collect references for this cluster
        let mut ep_refs: HashMap<u32, TemplateReference> = HashMap::new();
        let mut durations = Vec::new();

        for m_idx in &cluster {
            let m = &matches[*m_idx];
            let ev1 = evidence[m.first_idx];
            let ev2 = evidence[m.second_idx];

            let start1_ms = ev1.capture.window.start_ms + (m.segment.start1_sec * 1000.0) as i64;
            let end1_ms = ev1.capture.window.start_ms + (m.segment.end1_sec * 1000.0) as i64;

            let start2_ms = ev2.capture.window.start_ms + (m.segment.start2_sec * 1000.0) as i64;
            let end2_ms = ev2.capture.window.start_ms + (m.segment.end2_sec * 1000.0) as i64;

            durations.push((m.segment.duration_sec * 1000.0) as i64);

            let from_end1 = if kind == SegmentKind::Outro {
                ev1.duration_ms.map(|d| (d - end1_ms, d - start1_ms))
            } else {
                None
            };
            let from_end2 = if kind == SegmentKind::Outro {
                ev2.duration_ms.map(|d| (d - end2_ms, d - start2_ms))
            } else {
                None
            };

            ep_refs.entry(ev1.episode).or_insert_with(|| TemplateReference {
                sample_id: ev1.sample_id.clone(),
                ledger_id: ev1.ledger_id.clone(),
                episode: ev1.episode,
                source_version: ev1.source_version.clone(),
                match_interval_ms: (start1_ms, end1_ms),
                match_from_end_ms: from_end1,
            });

            ep_refs.entry(ev2.episode).or_insert_with(|| TemplateReference {
                sample_id: ev2.sample_id.clone(),
                ledger_id: ev2.ledger_id.clone(),
                episode: ev2.episode,
                source_version: ev2.source_version.clone(),
                match_interval_ms: (start2_ms, end2_ms),
                match_from_end_ms: from_end2,
            });
        }

        let references: Vec<TemplateReference> = ep_refs.into_values().collect();
        let unique_episodes = references.len();
        if unique_episodes < 2 {
            continue;
        }

        durations.sort_unstable();
        let expected_duration_ms = durations[durations.len() / 2];
        let is_stable = unique_episodes >= 3;

        template_models.push(TemplateModel {
            model_id: format!("{}-{}-{}", match kind {
                SegmentKind::Intro => "intro",
                SegmentKind::Outro => "outro",
            }, expected_duration_ms / 1000, idx),
            version: 1,
            kind,
            references,
            expected_duration_ms,
            is_stable,
        });

        if template_models.len() >= policy.max_templates_per_kind {
            break;
        }
    }

    template_models
}

fn cluster_matches(matches: &[PairCandidate], evidence: &[&EpisodeEvidence]) -> Vec<Vec<usize>> {
    let mut visited = vec![false; matches.len()];
    let mut clusters = Vec::new();

    for start in 0..matches.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut queue = vec![start];
        let mut current_cluster = Vec::new();

        while let Some(curr) = queue.pop() {
            current_cluster.push(curr);
            for cand in 0..matches.len() {
                if visited[cand] {
                    continue;
                }
                if are_candidates_consistent(&matches[curr], &matches[cand], evidence) {
                    visited[cand] = true;
                    queue.push(cand);
                }
            }
        }
        clusters.push(current_cluster);
    }

    clusters
}

fn are_candidates_consistent(
    a: &PairCandidate,
    b: &PairCandidate,
    evidence: &[&EpisodeEvidence],
) -> bool {
    // Check if they share an episode and have overlapping matching intervals
    for &(ep_a_idx, start_a, end_a) in &[
        (a.first_idx, a.segment.start1_sec, a.segment.end1_sec),
        (a.second_idx, a.segment.start2_sec, a.segment.end2_sec),
    ] {
        for &(ep_b_idx, start_b, end_b) in &[
            (b.first_idx, b.segment.start1_sec, b.segment.end1_sec),
            (b.second_idx, b.segment.start2_sec, b.segment.end2_sec),
        ] {
            if evidence[ep_a_idx].episode == evidence[ep_b_idx].episode {
                let overlap = (end_a.min(end_b) - start_a.max(start_b)).max(0.0);
                let shorter = (end_a - start_a).min(end_b - start_b);
                if shorter > 0.0 && overlap / shorter >= 0.8 {
                    return true;
                }
            }
        }
    }
    false
}
