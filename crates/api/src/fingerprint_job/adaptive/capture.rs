use std::path::PathBuf;
use std::time::SystemTime;

use marker::adaptive::{EpisodeEvidence, SegmentKind};
use marker::fingerprint::capture_types::{
    CaptureFailureKind, CaptureRequest, SampleWindow,
};
use store::{StoredFingerprintAttempt, StoredFingerprintSample};
use uuid::Uuid;

use super::cache::{find_reusable_sample, stored_sample_to_evidence};
use super::types::{AdaptiveCaptureContext, EpisodeCaptureRequest};

pub async fn capture_or_reuse_segment_sample(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
    phase: &str,
    attempt_count: &mut usize,
    max_budget: usize,
) -> Result<EpisodeEvidence, String> {
    // 1. Check cache first
    {
        let store = ctx.store.lock();
        if let Some(cached) = find_reusable_sample(&store, request, kind, window) {
            if let Some(timings) = &ctx.timings {
                match kind {
                    SegmentKind::Intro => timings.record_intro_cache_hit(Some(&request.job_id)),
                    SegmentKind::Outro => timings.record_outro_cache_hit(Some(&request.job_id)),
                }
            }
            tracing::info!(
                ledger_id = %request.row.id,
                kind = ?kind,
                start_ms = window.start_ms,
                end_ms = window.end_ms,
                "reused valid fingerprint sample from cache"
            );
            return Ok(cached);
        }
    }

    // 2. Perform live capture within budget
    if *attempt_count >= max_budget {
        return Err(format!(
            "Attempt budget exhausted ({}/{}) for episode {}",
            attempt_count, max_budget, request.row.id
        ));
    }

    *attempt_count += 1;
    let attempt_id = format!("att-{}", Uuid::new_v4());
    let kind_str = match kind {
        SegmentKind::Intro => "intro",
        SegmentKind::Outro => "outro",
    };

    let started_at_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let initial_attempt = StoredFingerprintAttempt {
        attempt_id: attempt_id.clone(),
        job_id: request.job_id.clone(),
        ledger_id: request.row.id.to_string(),
        kind: kind_str.to_string(),
        window_start_ms: window.start_ms,
        window_end_ms: window.end_ms,
        phase: phase.to_string(),
        started_at_ms,
        finished_at_ms: None,
        status: "running".to_string(),
        error_kind: None,
        metrics_json: "{}".to_string(),
    };

    {
        let store = ctx.store.lock();
        if let Err(e) = store.begin_fingerprint_attempt(&initial_attempt) {
            tracing::warn!(error = %e, "failed to record initial fingerprint attempt");
        }
    }

    // Rate gate
    ctx.gate.wait_before_capture().await;

    let capture_req = CaptureRequest {
        path: PathBuf::from(&request.row.path),
        window: window.clone(),
        audio_stream_index: request.audio_stream_index,
        process_deadline_ms: request.policy.process_deadline_ms,
    };

    let capture = ctx.capture.clone();
    let capture_res = if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::spawn_blocking(move || capture.capture_window(&capture_req))
            .await
            .map_err(|e| format!("Capture task panicked or failed: {e}"))?
    } else {
        capture.capture_window(&capture_req)
    };
    let finished_at_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    match capture_res {
        Ok(captured) => {
            let sample_id = format!("samp-{}", Uuid::new_v4());
            let mut sanitized_metrics = captured.metrics.clone();
            if let Some(ref tail) = sanitized_metrics.ffmpeg_stderr_tail {
                sanitized_metrics.ffmpeg_stderr_tail = Some(marker::fingerprint::sanitize_stderr(tail));
            }
            let metrics_json = serde_json::to_string(&sanitized_metrics).unwrap_or_else(|_| "{}".into());

            let sample = StoredFingerprintSample {
                sample_id: sample_id.clone(),
                ledger_id: request.row.id.to_string(),
                source_version: request.source_version.clone(),
                capture_profile_key: request.capture_profile_key.clone(),
                kind: kind_str.to_string(),
                window_start_ms: window.start_ms,
                window_end_ms: window.end_ms,
                pcm_duration_ms: captured.pcm_duration_ms,
                fingerprint: captured.words.clone(),
                captured_job_id: request.job_id.clone(),
                captured_at_ms: finished_at_ms,
                metrics_json: metrics_json.clone(),
            };

            let completed_attempt = StoredFingerprintAttempt {
                attempt_id,
                job_id: request.job_id.clone(),
                ledger_id: request.row.id.to_string(),
                kind: kind_str.to_string(),
                window_start_ms: window.start_ms,
                window_end_ms: window.end_ms,
                phase: phase.to_string(),
                started_at_ms,
                finished_at_ms: Some(finished_at_ms),
                status: "succeeded".to_string(),
                error_kind: None,
                metrics_json,
            };

            {
                let store = ctx.store.lock();
                if let Err(e) = store.complete_fingerprint_attempt(&completed_attempt, Some(&sample)) {
                    tracing::error!(error = %e, "failed to persist fingerprint attempt and sample");
                    return Err(format!("Store error completing attempt: {e}"));
                }
            }

            Ok(stored_sample_to_evidence(
                &sample,
                request.row.episode.unwrap_or(1),
                kind,
                request.media_duration_ms,
            ))
        }
        Err(err) => {
            let metrics_json = serde_json::to_string(&err.metrics).unwrap_or_else(|_| "{}".into());
            let error_kind_str = match err.kind {
                CaptureFailureKind::Io => "io",
                CaptureFailureKind::Timeout => "timeout",
                CaptureFailureKind::EmptyAudio => "empty_audio",
                CaptureFailureKind::InvalidWindow => "invalid_window",
                CaptureFailureKind::Decode => "decode",
            };

            let failed_attempt = StoredFingerprintAttempt {
                attempt_id,
                job_id: request.job_id.clone(),
                ledger_id: request.row.id.to_string(),
                kind: kind_str.to_string(),
                window_start_ms: window.start_ms,
                window_end_ms: window.end_ms,
                phase: phase.to_string(),
                started_at_ms,
                finished_at_ms: Some(finished_at_ms),
                status: "failed".to_string(),
                error_kind: Some(error_kind_str.to_string()),
                metrics_json,
            };

            {
                let store = ctx.store.lock();
                let _ = store.complete_fingerprint_attempt(&failed_attempt, None);
            }

            Err(format!("Capture failed: {}", err.message))
        }
    }
}
