//! Evidence collected from decoded audio. RMS is retained during fingerprint capture,
//! so examining the two bounded contexts requires no second media read.
use super::{BoundaryEvidence, BoundaryPolicy, Interval};
use crate::{CapturedFingerprint, SegmentKind};

#[derive(Clone, Copy, Debug)]
pub struct ReferenceSupport {
    pub references_agree: bool,
    pub complete_guards: bool,
}

pub fn complete_pcm(capture: &CapturedFingerprint) -> bool {
    if capture.metrics.time_mapping_version == 4 {
        return capture.window.validate().is_ok()
            && super::mapping::quantized_coverage(capture).is_some_and(|coverage| {
                coverage.start_ms <= capture.window.start_ms
                    && coverage.end_ms >= capture.window.end_ms
            });
    }
    !matches!(capture.metrics.pcm_timeline, crate::PcmTimeline::Invalid(_))
        && (capture.metrics.time_mapping_version < 3
            || (capture.metrics.pcm_timeline == crate::PcmTimeline::Contiguous
                && capture.metrics.contiguous_pcm_start_ms == Some(capture.window.start_ms)))
        && capture.window.validate().is_ok()
        && capture
            .metrics
            .contiguous_pcm_start_ms
            .is_none_or(|start| start == capture.window.start_ms)
        && capture.pcm_duration_ms.is_some_and(|duration| {
            duration >= 0
                && capture.window.start_ms.checked_add(duration).is_some()
                && duration
                    .checked_add(20)
                    .is_some_and(|end| end >= capture.window.duration_ms())
        })
}

/// Coverage on the existing normalized capture timeline. Complete captures retain
/// their measured contract; short captures need independently proven contiguous
/// origin. Duration alone cannot distinguish a missing suffix from leading loss.
pub fn candidate_pcm_coverage(capture: &CapturedFingerprint) -> Option<Interval> {
    if capture.metrics.time_mapping_version == 4 {
        capture.window.validate().ok()?;
        return super::mapping::quantized_coverage(capture)
            .filter(|coverage| coverage.end_ms > coverage.start_ms);
    }
    if matches!(capture.metrics.pcm_timeline, crate::PcmTimeline::Invalid(_)) {
        return None;
    }
    capture.window.validate().ok()?;
    if capture
        .metrics
        .contiguous_pcm_start_ms
        .is_some_and(|start| start != capture.window.start_ms)
        || (!complete_pcm(capture)
            && capture.metrics.contiguous_pcm_start_ms != Some(capture.window.start_ms))
    {
        return None;
    }
    let duration = capture.pcm_duration_ms.filter(|duration| *duration >= 0)?;
    let end_ms = capture
        .window
        .start_ms
        .checked_add(duration)?
        .min(capture.window.end_ms);
    Some(Interval {
        start_ms: capture.window.start_ms,
        end_ms,
    })
}

/// Whether the measured origin admits the requested first sample, for deciding
/// if another independent range may be searched. This is not complete PCM or
/// an absence proof. A physical head retains its independent zero-start facts.
pub fn capture_origin_covers_request_start(capture: &CapturedFingerprint) -> bool {
    if capture.window.validate().is_err() {
        return false;
    }
    let Some((min, max)) = super::mapping::quantized_origin(capture) else {
        return false;
    };
    if capture.window.start_ms == 0 {
        return super::mapping::quantized_head_start(capture).is_some();
    }
    let requested = i128::from(capture.window.start_ms) * 1000;
    min <= requested && requested <= max
}

pub fn complete_guards(
    capture: &CapturedFingerprint,
    candidate: Interval,
    _kind: SegmentKind,
    duration_ms: Option<i64>,
    policy: &BoundaryPolicy,
) -> bool {
    if policy.min_guard_ms < 0
        || candidate.start_ms < 0
        || candidate.end_ms <= candidate.start_ms
        || duration_ms.is_some_and(|duration| duration <= 0 || candidate.end_ms > duration)
    {
        return false;
    }
    let Some(coverage) = candidate_pcm_coverage(capture) else {
        return false;
    };
    let Some(left) = candidate.start_ms.checked_sub(policy.min_guard_ms) else {
        return false;
    };
    let Some(right) = candidate.end_ms.checked_add(policy.min_guard_ms) else {
        return false;
    };
    let required_start = left.max(0);
    let required_end = duration_ms.map_or(right, |duration| right.min(duration));
    let left_guard = coverage.start_ms <= required_start
        || (required_start == 0
            && coverage.start_ms <= candidate.start_ms
            && super::mapping::quantized_head_start(capture) == Some(coverage.start_ms));
    left_guard
        && capture.window.end_ms >= required_end
        && if capture.metrics.time_mapping_version == 4 {
            coverage.end_ms >= required_end
        } else {
            coverage
                .end_ms
                .checked_add(20)
                .is_some_and(|end| end >= required_end)
        }
}

pub fn collect_boundary_evidence(
    capture: &CapturedFingerprint,
    candidate: Interval,
    chapters: &[Interval],
    kind: SegmentKind,
    duration_ms: Option<i64>,
    support: ReferenceSupport,
    policy: &BoundaryPolicy,
) -> BoundaryEvidence {
    let near = |a: i64, b: i64| policy.context_ms >= 0 && a.abs_diff(b) <= policy.context_ms as u64;
    let source_chapters_present = !chapters.is_empty();
    let chapters: Vec<_> = chapters
        .iter()
        .copied()
        .filter(|chapter| {
            chapter.start_ms >= 0
                && chapter.end_ms > chapter.start_ms
                && near(chapter.start_ms, candidate.start_ms)
                && near(chapter.end_ms, candidate.end_ms)
        })
        .collect();
    let audio_transition = audio_boundaries(capture, candidate, policy);
    let complete_context = complete_guards(capture, candidate, kind, duration_ms, policy);
    BoundaryEvidence {
        candidate,
        complete_pcm: complete_pcm(capture),
        complete_context,
        chapter: chapters.first().copied(),
        audio_transition,
        references_agree: support.references_agree,
        complete_guards: support.complete_guards && complete_context,
        // RMS chatter carries no independent semantic contradiction. A source
        // chapter still needs a unique supported explanation for both bounds.
        conflicting_evidence: source_chapters_present && chapters.len() != 1,
    }
}

fn audio_boundaries(
    capture: &CapturedFingerprint,
    candidate: Interval,
    policy: &BoundaryPolicy,
) -> Option<Interval> {
    let frame_ms = capture.metrics.pcm_frame_ms;
    if frame_ms <= 0 || frame_ms != policy.pcm_frame_ms || policy.transition_window_ms <= 0 {
        return None;
    }
    let frames = &capture.metrics.pcm_rms_dbfs_milli;
    let transitions = transition_points(frames, frame_ms, policy);
    let start = unique_boundary(
        &transitions,
        candidate.start_ms,
        capture.window.start_ms,
        policy,
    );
    let end = unique_boundary(
        &transitions,
        candidate.end_ms,
        capture.window.start_ms,
        policy,
    );
    start.zip(end).and_then(|(start_ms, end_ms)| {
        super::normalize_capture_interval(
            capture,
            Interval {
                start_ms: start_ms - capture.window.start_ms,
                end_ms: end_ms - capture.window.start_ms,
            },
        )
        .ok()
    })
}

fn unique_boundary(
    points: &[i64],
    endpoint: i64,
    origin: i64,
    policy: &BoundaryPolicy,
) -> Option<i64> {
    let mut near: Vec<_> = points
        .iter()
        .filter_map(|point| origin.checked_add(*point))
        .filter(|point| {
            policy.context_ms >= 0 && point.abs_diff(endpoint) <= policy.context_ms as u64
        })
        .collect();
    near.sort_unstable();
    if near
        .first()
        .zip(near.last())
        .is_some_and(|(first, last)| first.abs_diff(*last) > policy.agreement_ms.max(0) as u64)
    {
        return None;
    }
    near.get(near.len() / 2).copied()
}

fn transition_points(frames: &[i32], frame_ms: i64, policy: &BoundaryPolicy) -> Vec<i64> {
    let count = (policy.transition_window_ms / frame_ms).max(1) as usize;
    let silence_count = (policy.silence_duration_ms / frame_ms).max(1) as usize;
    let power = |db: i32| 10f64.powf(f64::from(db) / 10_000.0);
    let mut points = Vec::new();
    for index in count..frames.len().saturating_sub(count) {
        let rms_db = |range: &[i32]| {
            10.0 * (range.iter().map(|db| power(*db)).sum::<f64>() / range.len() as f64)
                .max(1e-12)
                .log10()
        };
        let left = rms_db(&frames[index - count..index]);
        let right = rms_db(&frames[index..index + count]);
        let silence_edge = index >= silence_count && index + silence_count <= frames.len() && {
            let silent = |slice: &[i32]| {
                slice
                    .iter()
                    .all(|db| f64::from(*db) / 1000.0 < policy.silence_dbfs)
            };
            silent(&frames[index - silence_count..index])
                != silent(&frames[index..index + silence_count])
        };
        if (left - right).abs() >= policy.transition_db || silence_edge {
            points.push(index as i64 * frame_ms);
        }
    }
    points
}

/// Review a previously selected candidate using two independent complete references.
/// Coordinates and guards on both sides come from decoded PCM, never word count.
pub fn review_candidate(
    engine: &dyn crate::FingerprintEngine,
    target: &crate::EpisodeEvidence,
    references: &[&crate::EpisodeEvidence],
    candidate: Interval,
    chapters: &[Interval],
    policy: &crate::SamplingPolicy,
) -> super::BoundaryDecision {
    let mut supporting = std::collections::HashSet::new();
    let mut guarded = std::collections::HashSet::new();
    for reference in references {
        if reference.kind != target.kind
            || reference.episode == target.episode
            || candidate_pcm_coverage(&reference.capture).is_none()
            || crate::adaptive::model::is_constant_or_silent(&reference.capture.words)
        {
            continue;
        }
        let decision = engine.compare_evidence(
            target,
            reference,
            policy.min_match_duration_ms as f32 / 1000.0,
            policy.max_match_duration_ms as f32 / 1000.0,
        );
        let crate::PairFingerprintDecision::Candidates(segments) = decision else {
            continue;
        };
        for segment in segments {
            if !crate::candidate_quality::acceptable_segment(
                &segment,
                policy.min_match_duration_ms as f32 / 1000.0,
                policy.max_match_duration_ms as f32 / 1000.0,
                policy.max_score,
            ) {
                continue;
            }
            let relative = |start: f32, end: f32| Interval {
                start_ms: (start * 1000.0).round() as i64,
                end_ms: (end * 1000.0).round() as i64,
            };
            let Ok(target_interval) = super::normalize_capture_interval(
                &target.capture,
                relative(segment.start1_sec, segment.end1_sec),
            ) else {
                continue;
            };
            let Ok(ref_interval) = super::normalize_capture_interval(
                &reference.capture,
                relative(segment.start2_sec, segment.end2_sec),
            ) else {
                continue;
            };
            if target_interval.start_ms.abs_diff(candidate.start_ms)
                > policy.boundary.agreement_ms.max(0) as u64
                || target_interval.end_ms.abs_diff(candidate.end_ms)
                    > policy.boundary.agreement_ms.max(0) as u64
            {
                continue;
            }
            supporting.insert(reference.episode);
            if complete_guards(
                &reference.capture,
                ref_interval,
                reference.kind,
                reference.duration_ms,
                &policy.boundary,
            ) {
                guarded.insert(reference.episode);
            }
        }
    }
    let evidence = collect_boundary_evidence(
        &target.capture,
        candidate,
        chapters,
        target.kind,
        target.duration_ms,
        ReferenceSupport {
            references_agree: supporting.len() >= 2,
            complete_guards: guarded.len() >= 2,
        },
        &policy.boundary,
    );
    super::refine_boundary(&evidence, &policy.boundary)
}
