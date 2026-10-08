use marker::adaptive::{
    verify_template_window, EpisodeEvidence, VerificationOutcome,
};
use marker::FingerprintEngine;

use super::types::EpisodeCaptureRequest;

pub fn analyze_and_verify_segment(
    matcher: &dyn FingerprintEngine,
    request: &EpisodeCaptureRequest,
    evidence: &EpisodeEvidence,
) -> VerificationOutcome {
    if request.templates.models.is_empty() {
        return VerificationOutcome::NeedsFullWindow {
            reason: "no_templates_available".to_string(),
        };
    }

    verify_template_window(matcher, evidence, &request.templates, &request.policy)
}
