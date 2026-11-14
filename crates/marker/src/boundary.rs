//! Conservative checks for candidate boundaries on the actual capture timeline.
pub mod evidence;
mod mapping;
pub use mapping::normalize_capture_interval;

/// Increment when boundary evidence semantics change without changing captures.
pub const EVIDENCE_POLICY_VERSION: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BoundaryPolicy {
    pub pcm_frame_ms: i64,
    pub silence_dbfs: f64,
    pub silence_duration_ms: i64,
    pub transition_db: f64,
    pub transition_window_ms: i64,
    pub context_ms: i64,
    pub agreement_ms: i64,
    pub min_guard_ms: i64,
}

impl Default for BoundaryPolicy {
    fn default() -> Self {
        Self {
            pcm_frame_ms: 20,
            silence_dbfs: -45.0,
            silence_duration_ms: 200,
            transition_db: 12.0,
            transition_window_ms: 200,
            context_ms: 10_000,
            agreement_ms: 1_000,
            min_guard_ms: 3_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BoundaryEvidence {
    pub candidate: Interval,
    pub complete_pcm: bool,
    /// Actual candidate and bounded guard context are covered on its timeline.
    pub complete_context: bool,
    pub chapter: Option<Interval>,
    pub audio_transition: Option<Interval>,
    pub references_agree: bool,
    pub complete_guards: bool,
    pub conflicting_evidence: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundaryDecision {
    Verified(Interval),
    Uncertain(&'static str),
}

pub fn normalize_interval(
    window: &crate::SampleWindow,
    relative: Interval,
) -> Result<Interval, String> {
    window.validate().map_err(|error| error.to_string())?;
    if relative.start_ms < 0
        || relative.end_ms <= relative.start_ms
        || relative.end_ms > window.duration_ms()
    {
        return Err("relative interval is outside the actual capture window".into());
    }
    Ok(Interval {
        start_ms: window
            .start_ms
            .checked_add(relative.start_ms)
            .ok_or("capture start overflow")?,
        end_ms: window
            .start_ms
            .checked_add(relative.end_ms)
            .ok_or("capture end overflow")?,
    })
}

pub fn refine_boundary(e: &BoundaryEvidence, policy: &BoundaryPolicy) -> BoundaryDecision {
    if !e.complete_context || !e.complete_guards || !e.references_agree || e.conflicting_evidence {
        return BoundaryDecision::Uncertain("incomplete_or_conflicting_boundary_evidence");
    }
    let valid = |i: Interval| {
        i.start_ms >= 0
            && i.end_ms
                .checked_sub(i.start_ms)
                .is_some_and(|duration| duration >= 15_000)
    };
    if !valid(e.candidate) {
        return BoundaryDecision::Uncertain("invalid_candidate_interval");
    }
    match (e.chapter, e.audio_transition) {
        (Some(chapter), Some(audio)) => {
            let near = |a: i64, b: i64, limit: i64| limit >= 0 && a.abs_diff(b) <= limit as u64;
            if !near(chapter.start_ms, audio.start_ms, policy.agreement_ms)
                || !near(chapter.end_ms, audio.end_ms, policy.agreement_ms)
                || !near(chapter.start_ms, e.candidate.start_ms, policy.context_ms)
                || !near(audio.start_ms, e.candidate.start_ms, policy.context_ms)
                || !near(chapter.end_ms, e.candidate.end_ms, policy.context_ms)
                || !near(audio.end_ms, e.candidate.end_ms, policy.context_ms)
            {
                return BoundaryDecision::Uncertain("auxiliary_boundaries_disagree");
            }
            let narrowed = Interval {
                start_ms: chapter.start_ms.max(audio.start_ms),
                end_ms: chapter.end_ms.min(audio.end_ms),
            };
            if narrowed.start_ms < e.candidate.start_ms
                || narrowed.end_ms > e.candidate.end_ms
                || !valid(narrowed)
            {
                return BoundaryDecision::Uncertain("refinement_would_expand_or_empty_candidate");
            }
            BoundaryDecision::Verified(narrowed)
        }
        // Raw audio observations cannot refine bounds without source chapters.
        (None, _) if e.complete_guards => BoundaryDecision::Verified(e.candidate),
        _ => BoundaryDecision::Uncertain("missing_corroborating_boundary_evidence"),
    }
}
