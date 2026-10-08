pub mod analysis;
pub mod cache;
pub mod capture;
pub mod types;

use marker::adaptive::{
    plan_episode_window, EpisodeDescriptor, EpisodeEvidence, SegmentKind, VerificationOutcome,
    VerifiedInterval, WindowDecision,
};
use marker::fingerprint::capture_types::SampleWindow;

pub use analysis::analyze_and_verify_segment;
pub use cache::{find_reusable_sample, stored_sample_to_evidence};
pub use capture::capture_or_reuse_segment_sample;
pub use types::{
    AdaptiveCaptureContext, CaptureGate, CapturePolicy, EpisodeCaptureRequest, EpisodeDetection,
};

pub struct NoopGate;

impl CaptureGate for NoopGate {
    fn wait_before_capture(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

pub async fn capture_episode_adaptive(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
) -> Result<EpisodeDetection, String> {
    let mut intro_attempts_used = 0;
    let mut outro_attempts_used = 0;

    let descriptor = EpisodeDescriptor {
        ledger_id: request.row.id.to_string(),
        episode: request.row.episode.unwrap_or(1),
        source_version: request.source_version.clone(),
        duration_ms: request.media_duration_ms.unwrap_or(0),
    };

    // 1. Process intro with its own attempt budget
    let (intro_evidence, intro_match) = process_segment(
        ctx,
        request,
        &descriptor,
        SegmentKind::Intro,
        &mut intro_attempts_used,
    )
    .await?;

    // 2. Process outro with its own attempt budget (if duration is known)
    let (outro_evidence, outro_match) = if request.media_duration_ms.is_some() {
        process_segment(
            ctx,
            request,
            &descriptor,
            SegmentKind::Outro,
            &mut outro_attempts_used,
        )
        .await?
    } else {
        (None, None)
    };

    Ok(EpisodeDetection {
        ledger_id: request.row.id.to_string(),
        episode: request.row.episode.unwrap_or(1),
        intro_evidence,
        outro_evidence,
        intro_match,
        outro_match,
    })
}

async fn process_segment(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    descriptor: &EpisodeDescriptor,
    kind: SegmentKind,
    attempts: &mut usize,
) -> Result<(Option<EpisodeEvidence>, Option<VerifiedInterval>), String> {
    let decision = plan_episode_window(
        descriptor,
        kind,
        &request.templates,
        &request.policy,
        &request.cost_summary,
    );

    let max_budget = request.policy.max_attempts_per_kind as usize;

    match decision {
        WindowDecision::Full { window, .. } => {
            let ev = capture_with_retries(ctx, request, kind, &window, "full_window", attempts, max_budget)
                .await?;
            let outcome = analyze_and_verify_segment(ctx.matcher.as_ref(), request, &ev);
            let verified = match outcome {
                VerificationOutcome::Verified(v) => Some(v),
                _ => None,
            };
            Ok((Some(ev), verified))
        }
        WindowDecision::Verify { window, .. } => {
            // First try verification window (up to max_budget - 1 attempts, reserving at least 1 for fallback)
            let verify_budget = max_budget.saturating_sub(1).max(1);
            let verify_ev = match capture_with_retries(ctx, request, kind, &window, "fast_verify", attempts, verify_budget).await {
                Ok(ev) => ev,
                Err(err) => {
                    tracing::warn!(error = %err, "fast verify capture failed, trying full window fallback");
                    let fallback_win = full_window_for_kind(descriptor.duration_ms, request.policy.full_window_duration_secs, kind);
                    let fb_ev = capture_with_retries(ctx, request, kind, &fallback_win, "fallback_full_window", attempts, max_budget).await?;
                    let outcome = analyze_and_verify_segment(ctx.matcher.as_ref(), request, &fb_ev);
                    let verified = match outcome {
                        VerificationOutcome::Verified(v) => Some(v),
                        _ => None,
                    };
                    return Ok((Some(fb_ev), verified));
                }
            };

            let outcome = analyze_and_verify_segment(ctx.matcher.as_ref(), request, &verify_ev);
            match outcome {
                VerificationOutcome::Verified(v) => Ok((Some(verify_ev), Some(v))),
                VerificationOutcome::NeedsFullWindow { reason } => {
                    tracing::info!(reason = %reason, "verification requested full window fallback");
                    let fallback_win = full_window_for_kind(descriptor.duration_ms, request.policy.full_window_duration_secs, kind);
                    let fb_ev = capture_with_retries(ctx, request, kind, &fallback_win, "fallback_full_window", attempts, max_budget).await?;
                    let outcome2 = analyze_and_verify_segment(ctx.matcher.as_ref(), request, &fb_ev);
                    let verified2 = match outcome2 {
                        VerificationOutcome::Verified(v) => Some(v),
                        _ => None,
                    };
                    Ok((Some(fb_ev), verified2))
                }
                VerificationOutcome::NoMatch { .. } | VerificationOutcome::SamplingLimit { .. } => {
                    Ok((Some(verify_ev), None))
                }
            }
        }
    }
}

async fn capture_with_retries(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
    phase: &str,
    attempts: &mut usize,
    max_step_retries: usize,
) -> Result<EpisodeEvidence, String> {
    let mut last_err = String::new();
    let mut step_attempts = 0;
    let max_budget = request.policy.max_attempts_per_kind as usize;

    while *attempts < max_budget && step_attempts < max_step_retries {
        step_attempts += 1;
        match capture_or_reuse_segment_sample(
            ctx,
            request,
            kind,
            window,
            phase,
            attempts,
            max_budget,
        )
        .await
        {
            Ok(ev) => return Ok(ev),
            Err(e) => {
                last_err = e;
            }
        }
    }

    if *attempts >= max_budget && last_err.is_empty() {
        return Err(format!("Total attempt budget exhausted ({max_budget})"));
    }

    Err(last_err)
}

fn full_window_for_kind(duration_ms: i64, full_secs: u32, kind: SegmentKind) -> SampleWindow {
    let full_ms = (full_secs as i64) * 1000;
    match kind {
        SegmentKind::Intro => SampleWindow::new(0, full_ms.max(1000)),
        SegmentKind::Outro => {
            let start = (duration_ms - full_ms).max(0);
            SampleWindow::new(start, duration_ms.max(start + 1000))
        }
    }
}
