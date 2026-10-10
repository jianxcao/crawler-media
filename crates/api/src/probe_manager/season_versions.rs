use crate::fingerprint_job::EpisodeDetection;
use marker::adaptive::VerificationOutcome;

/// Every version participates, including versions with no marker in either kind.
pub(super) fn validate_version_outcomes(detections: &[EpisodeDetection]) -> Result<(), String> {
    for (index, first) in detections.iter().enumerate() {
        for second in &detections[index + 1..] {
            if first.episode != second.episode {
                continue;
            }
            for (kind, left, right) in [
                ("intro", &first.intro_outcome, &second.intro_outcome),
                ("outro", &first.outro_outcome, &second.outro_outcome),
            ] {
                if conflicting(left.as_ref(), right.as_ref()) {
                    return Err(format!(
                        "episode_version_conflict: episode={} kind={kind} ledgers={} vs {}",
                        first.episode, first.ledger_id, second.ledger_id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn conflicting(left: Option<&VerificationOutcome>, right: Option<&VerificationOutcome>) -> bool {
    use VerificationOutcome::{NoMatch, Verified};
    match (left, right) {
        (Some(Verified(a)), Some(Verified(b))) => {
            (a.start_ms - b.start_ms).abs() > 1000 || (a.end_ms - b.end_ms).abs() > 1000
        }
        (Some(Verified(_)), Some(NoMatch { .. })) | (Some(NoMatch { .. }), Some(Verified(_))) => {
            true
        }
        _ => false,
    }
}
