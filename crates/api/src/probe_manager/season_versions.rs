use crate::fingerprint_job::EpisodeDetection;
use marker::adaptive::SegmentKind;

/// No-match decisions cannot remove an older interval outside decoded coverage.
pub(super) fn validate_previous_absence(
    previous: Option<&store::StoredFileMarker>,
    detection: Option<&EpisodeDetection>,
) -> Result<(), String> {
    if let (Some(previous), Some(detection)) = (previous, detection) {
        for (kind, outcome, evidence) in [
            (
                SegmentKind::Intro,
                &detection.intro_outcome,
                &detection.intro_evidence,
            ),
            (
                SegmentKind::Outro,
                &detection.outro_outcome,
                &detection.outro_evidence,
            ),
        ] {
            if matches!(outcome, Some(marker::VerificationOutcome::NoMatch { .. })) {
                super::super::markers::validate_absence_coverage(
                    previous,
                    kind,
                    evidence.as_ref().map(|e| &e.capture),
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}
