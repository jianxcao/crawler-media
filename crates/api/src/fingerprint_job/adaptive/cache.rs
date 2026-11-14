use marker::adaptive::{EpisodeEvidence, SegmentKind};
use marker::fingerprint::capture_types::{CaptureMetrics, CapturedFingerprint, SampleWindow};
use store::{FingerprintSampleQuery, Store, StoredFingerprintSample};

use super::types::{CapturePolicy, EpisodeCaptureRequest};
use crate::fingerprint_job::episode_number;

pub fn find_reusable_sample(
    store: &Store,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
) -> Option<EpisodeEvidence> {
    find_reusable_sample_bounded(store, request, kind, window, None)
}

pub(crate) fn find_reusable_sample_bounded(
    store: &Store,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
    max_window_ms: Option<i64>,
) -> Option<EpisodeEvidence> {
    if request.capture_policy == CapturePolicy::Recapture && request.job_id.is_empty() {
        return None;
    }
    let kind_str = match kind {
        SegmentKind::Intro => "intro",
        SegmentKind::Outro => "outro",
    };

    let query = FingerprintSampleQuery {
        ledger_id: request.row.file.file_id.to_string(),
        source_version: request.source_version.clone(),
        capture_profile_key: request.capture_profile_key.clone(),
        kind: kind_str.to_string(),
        window_start_ms: window.start_ms,
        window_end_ms: window.end_ms,
        captured_job_id: (request.capture_policy == CapturePolicy::Recapture)
            .then(|| request.job_id.clone()),
    };

    let source = crate::fingerprint_job::source_for_file_selected(
        store,
        &request.row.file,
        request.audio_stream_index,
    )?;
    let mut samples = store.find_covering_fingerprint_samples(&query).ok()?;
    if max_window_ms.is_some() {
        // A durable receipt from this job wins over unrelated global cache rows,
        // including a newer row with identical geometry after a process restart.
        samples.sort_by_key(|sample| {
            (
                sample.captured_job_id != request.job_id,
                sample.window_start_ms != window.start_ms || sample.window_end_ms != window.end_ms,
            )
        });
    }
    let matched = samples.into_iter().find(|s| {
        let capture = stored_sample_to_evidence(s, 0, kind, request.media_duration_ms).capture;
        // Reuse the actual enclosing window, never relabel its PCM as the requested subwindow.
        max_window_ms.is_none_or(|bound| s.window_end_ms - s.window_start_ms <= bound)
            && s.window_start_ms <= window.start_ms
            && s.window_end_ms >= window.end_ms
            && crate::fingerprint_job::reusable_capture(
                &capture,
                &request.capture_profile_key,
                &capture.window,
                Some(&source),
            )
    })?;

    Some(stored_sample_to_evidence(
        &matched,
        episode_number(&request.row).ok()?,
        kind,
        request.media_duration_ms,
    ))
}

pub fn stored_sample_to_evidence(
    stored: &StoredFingerprintSample,
    episode: u32,
    kind: SegmentKind,
    duration_ms: Option<i64>,
) -> EpisodeEvidence {
    let metrics: CaptureMetrics = serde_json::from_str(&stored.metrics_json).unwrap_or_default();
    EpisodeEvidence {
        sample_id: stored.sample_id.clone(),
        ledger_id: stored.ledger_id.clone(),
        episode,
        source_version: stored.source_version.clone(),
        capture_profile_key: stored.capture_profile_key.clone(),
        kind,
        capture: CapturedFingerprint {
            window: SampleWindow {
                start_ms: stored.window_start_ms,
                end_ms: stored.window_end_ms,
            },
            words: stored.fingerprint.clone(),
            pcm_duration_ms: stored.pcm_duration_ms,
            metrics,
        },
        duration_ms,
    }
}
