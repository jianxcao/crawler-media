use marker::adaptive::{EpisodeEvidence, SegmentKind};
use marker::fingerprint::capture_types::{CaptureMetrics, CapturedFingerprint, SampleWindow};
use store::{FingerprintSampleQuery, Store, StoredFingerprintSample};

use super::types::{CapturePolicy, EpisodeCaptureRequest};

pub fn find_reusable_sample(
    store: &Store,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
) -> Option<EpisodeEvidence> {
    if request.capture_policy == CapturePolicy::Recapture {
        return None;
    }

    let kind_str = match kind {
        SegmentKind::Intro => "intro",
        SegmentKind::Outro => "outro",
    };

    let query = FingerprintSampleQuery {
        ledger_id: request.row.id.to_string(),
        source_version: request.source_version.clone(),
        capture_profile_key: request.capture_profile_key.clone(),
        kind: kind_str.to_string(),
        window_start_ms: window.start_ms,
        window_end_ms: window.end_ms,
        captured_job_id: None,
    };

    let samples = store.find_covering_fingerprint_samples(&query).ok()?;
    let matched = samples.into_iter().find(|s| {
        s.window_start_ms <= window.start_ms && s.window_end_ms >= window.end_ms
    })?;

    Some(stored_sample_to_evidence(&matched, request.row.episode.unwrap_or(1), kind))
}

pub fn stored_sample_to_evidence(
    stored: &StoredFingerprintSample,
    episode: u32,
    kind: SegmentKind,
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
    }
}
