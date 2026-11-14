use crate::store::{StoreError, StoredFileMarker};
use marker::SegmentKind;

/// A completed comparison proves absence only inside its actual decoded window.
/// Never delete an existing range that this comparison did not observe.
pub(in crate::probe_manager) fn validate_absence_coverage(
    previous: &StoredFileMarker,
    kind: SegmentKind,
    capture: Option<&marker::CapturedFingerprint>,
) -> Result<(), StoreError> {
    if previous.locked {
        return Ok(());
    }
    let bounds = match kind {
        SegmentKind::Intro => (previous.intro_start_ms, previous.intro_end_ms),
        SegmentKind::Outro => (previous.outro_start_ms, previous.outro_end_ms),
    };
    if bounds == (None, None) {
        return Ok(());
    }
    if let (Some(start), Some(end), Some(capture)) = (bounds.0, bounds.1, capture)
        && marker::boundary::evidence::complete_pcm(capture)
        && start >= capture.window.start_ms
        && end > start
        && capture
            .pcm_duration_ms
            .and_then(|duration| capture.window.start_ms.checked_add(duration))
            .is_some_and(|decoded_end| end <= decoded_end.min(capture.window.end_ms))
    {
        return Ok(());
    }
    Err(StoreError::Invalid(format!(
        "verification_unresolved: episode={} kind={kind:?} previous_interval_outside_complete_capture",
        previous.unit.episode
    )))
}
