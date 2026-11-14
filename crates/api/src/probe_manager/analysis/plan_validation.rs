//! Validate only the supported persisted capture contract before scheduling IO.
use super::{capture::CapturePlan, intro_search::KindSearch};
use marker::{SampleWindow, SamplingPolicy, SegmentKind};
use std::collections::HashSet;
use store::StoreError;

fn invalid(reason: &str) -> StoreError {
    StoreError::Invalid(format!("invalid_capture_plan: {reason}"))
}
impl CapturePlan {
    pub(super) fn consume(
        manager: &super::ProbeManager,
        unit: &super::ProbeUnit,
        encoded: &str,
        duration: Option<i64>,
    ) -> Result<Option<Self>, StoreError> {
        match Self::parse(encoded, duration) {
            Ok(plan) => Ok(Some(plan)),
            Err(error @ (StoreError::Json(_) | StoreError::Invalid(_))) => {
                let reason = format!("invalid_capture_plan: {error}");
                tracing::error!(%error,job_id=?unit.job_id,file_id=%unit.row.file.file_id,"持久采集计划消费失败，终止该单元");
                manager.finish_with_status(
                    unit,
                    "failed",
                    Some("invalid_capture_plan"),
                    Some(&reason),
                );
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn parse(encoded: &str, duration: Option<i64>) -> Result<Self, StoreError> {
        let plan: Self = serde_json::from_str(encoded)?;
        plan.validate(duration)?;
        Ok(plan)
    }
    pub(super) fn validate(&self, duration: Option<i64>) -> Result<(), StoreError> {
        if self.policy.full_window_duration_secs == 0 {
            return Err(invalid("empty initial window"));
        }
        validate_evidence_policy(&self.policy)?;
        match self.sampling_mode.as_str() {
            // These quotas are unused by full-window capture, whose configured
            // sample duration can differ from adaptive discovery's initial180s.
            "full_window" => {
                return if self.searches.is_empty() && self.selected_samples.is_empty() {
                    Ok(())
                } else {
                    Err(invalid("adaptive state in full-window plan"))
                };
            }
            "adaptive" => {}
            _ => return Err(invalid("unsupported sampling mode")),
        }
        validate_adaptive_policy(&self.policy)?;
        let mut kinds = HashSet::new();
        for search in &self.searches {
            if !kinds.insert(search.kind) {
                return Err(invalid("duplicate kind"));
            }
            validate_search(
                search,
                &self.policy,
                duration.filter(|duration| *duration > 0),
            )?;
        }
        let unique = self.selected_samples.iter().collect::<HashSet<_>>();
        if unique.len() != self.selected_samples.len()
            || unique.iter().any(|id| id.is_empty())
            || self.selected_samples.len() > self.searches.len() * self.policy.max_windows_per_kind
        {
            return Err(invalid("unbounded or duplicate selected samples"));
        }
        Ok(())
    }
}
// Both capture modes feed these fields into candidate/boundary review. They
// cannot bypass validation just because adaptive capture quotas are unused.
fn validate_evidence_policy(policy: &SamplingPolicy) -> Result<(), StoreError> {
    if policy.min_match_duration_ms < 15_000
        || policy.max_match_duration_ms > 240_000
        || policy.min_match_duration_ms > policy.max_match_duration_ms
        || !policy.max_score.is_finite()
        || !(0.0..=4.0).contains(&policy.max_score)
        || policy.boundary.min_guard_ms < 3_000
    {
        return Err(invalid("invalid shared evidence policy"));
    }
    Ok(())
}

fn validate_adaptive_policy(policy: &SamplingPolicy) -> Result<(), StoreError> {
    if !(1..=3).contains(&policy.max_windows_per_kind)
        || !(1..=4).contains(&policy.max_attempts_per_kind)
        || policy.process_deadline_ms == 0
        || policy.process_deadline_ms > 120_000
        || policy.full_window_duration_secs != 180
        || policy.seed_count < 3
        || policy.seed_count > policy.max_seed_count
        || policy.max_seed_count > 5
        || !(1..=3).contains(&policy.max_templates_per_kind)
    {
        return Err(invalid("unsupported adaptive sampling policy"));
    }
    if policy.context_margin_ms < 0
        || policy.max_reference_boundary_delta_ms < 0
        || !policy.min_reference_coverage.is_finite()
        || !(0.95..=1.0).contains(&policy.min_reference_coverage)
        || !policy.min_window_saving_ratio.is_finite()
        || !(0.0..=1.0).contains(&policy.min_window_saving_ratio)
        || policy.min_guard_evidence_ms < 3_000
        || policy.max_internal_gap_ms < 0
        || policy.template_edge_anchor_ms < 0
    {
        return Err(invalid("invalid adaptive evidence policy"));
    }
    Ok(())
}
fn validate_search(
    search: &KindSearch,
    policy: &SamplingPolicy,
    duration: Option<i64>,
) -> Result<(), StoreError> {
    let limit = if search.kind == SegmentKind::Outro {
        policy.max_windows_per_kind.min(2)
    } else {
        policy.max_windows_per_kind
    };
    if search.windows.len() > limit
        || search.pending.is_some() && search.terminal_reason.is_some()
        || search.windows.is_empty() && search.pending.is_none() && search.terminal_reason.is_none()
    {
        return Err(invalid("contradictory search state"));
    }
    for (index, window) in search.windows.iter().enumerate() {
        validate_window(window, search.kind, duration)?;
        if search.windows[..index].contains(window) {
            return Err(invalid("duplicate window"));
        }
    }
    if let Some(window) = &search.pending {
        validate_window(window, search.kind, duration)?;
        if !search.windows.contains(window) && search.windows.len() >= limit {
            return Err(invalid("pending window exceeds budget"));
        }
    }
    Ok(())
}
fn validate_window(
    window: &SampleWindow,
    kind: SegmentKind,
    duration: Option<i64>,
) -> Result<(), StoreError> {
    if window.validate().is_err()
        || window.duration_ms() > 300_000
        || duration.is_some_and(|duration| window.end_ms > duration)
        || kind == SegmentKind::Intro && window.end_ms > 600_000
        || kind == SegmentKind::Outro
            && duration.is_none_or(|duration| window.start_ms < (duration - 300_000).max(0))
    {
        return Err(invalid("window outside bounded kind search"));
    }
    Ok(())
}
