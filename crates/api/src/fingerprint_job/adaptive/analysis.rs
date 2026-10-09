use marker::adaptive::{
    verify_template_window, EpisodeEvidence, VerificationOutcome,
};
use marker::FingerprintEngine;

use super::types::EpisodeCaptureRequest;

pub fn analyze_and_verify_segment(
    matcher: &dyn FingerprintEngine,
    request: &EpisodeCaptureRequest,
    evidence: &EpisodeEvidence,
    timings: Option<&crate::probe_manager::timings::ProbeTimingLedger>,
) -> VerificationOutcome {
    if request.templates.models.is_empty() {
        return VerificationOutcome::NeedsFullWindow {
            reason: "no_templates_available".to_string(),
        };
    }

    let start = std::time::Instant::now();
    let outcome = verify_template_window(matcher, evidence, &request.templates, &request.policy);
    if let Some(timings) = timings {
        timings.record_comparison(Some(&request.job_id), start.elapsed().as_millis() as u64);
    }
    outcome
}
