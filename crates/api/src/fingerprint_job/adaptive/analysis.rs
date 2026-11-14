use marker::adaptive::{EpisodeEvidence, VerificationOutcome, verify_template_window};
use marker::matcher::session::SeasonMatchSession;

use super::types::EpisodeCaptureRequest;

pub fn analyze_and_verify_segment(
    matcher: &std::sync::Arc<SeasonMatchSession>,
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
    let (outcome, observation) = crate::fingerprint_job::measurement::EvidenceMeasurement::new(
        matcher.clone(),
        &request.policy,
    )
    .measure(|engine| {
        verify_template_window(engine, evidence, &request.templates, &request.policy)
    });
    if let Some(timings) = timings {
        timings.record_analysis(Some(&request.job_id), &observation);
        timings.record_comparison(Some(&request.job_id), start.elapsed().as_millis() as u64);
    }
    outcome
}
