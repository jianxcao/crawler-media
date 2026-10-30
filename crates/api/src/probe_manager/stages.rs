use store::{ProbeStage, ProbeStageCompletion, ProbeStageKey};

use super::ProbeManager;

pub(super) fn log_fingerprint_publication(
    unit: &super::ProbeUnit,
    ledger_id: &str,
    fp_path: &std::path::Path,
    source_version: &str,
    outcome: &crate::fingerprint_job::FingerprintCaptureOutcome,
    started: std::time::Instant,
) {
    tracing::info!(
        ledger_id,
        media_id = %unit.row.file.media_id,
        season = ?super::single_season(&unit.row),
        episode = ?super::single_episode(&unit.row),
        path = %fp_path.display(),
        source_version,
        cache_key = outcome.cache.cache_key,
        intro_cache_hit = outcome.intro_cache_hit,
        outro_cache_hit = outcome.outro_cache_hit,
        intro_words = outcome.cache.intro.len(),
        outro_words = outcome.cache.outro.as_ref().map(Vec::len).unwrap_or_default(),
        intro_elapsed_ms = outcome.intro_elapsed_ms,
        outro_elapsed_ms = outcome.outro_elapsed_ms,
        media_duration_ms = ?outcome.cache.media_duration_ms,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "【声纹】版本化指纹缓存已持久化"
    );
}

pub(super) fn record_fingerprint_stages(
    mgr: &ProbeManager,
    ledger_id: &str,
    job_id: &str,
    cache_key: &str,
    outro_error: Option<&str>,
    now_ms: i64,
) -> Result<(), crate::store::StoreError> {
    let intro_key = ProbeStageKey {
        ledger_id: ledger_id.to_string(),
        context_key: cache_key.to_string(),
        stage: ProbeStage::Intro,
    };
    let store = mgr.writer()?;
    store.adopt_probe_context(ledger_id, cache_key)?;
    store.claim_probe_stage(&intro_key, job_id, now_ms)?;
    store.finish_probe_stage(&intro_key, job_id, &ProbeStageCompletion::Succeeded, now_ms)?;
    let outro_key = ProbeStageKey {
        ledger_id: ledger_id.to_string(),
        context_key: cache_key.to_string(),
        stage: ProbeStage::Outro,
    };
    store.claim_probe_stage(&outro_key, job_id, now_ms)?;
    let completion = match outro_error {
        Some(error) => ProbeStageCompletion::Failed {
            error_kind: classify_fingerprint_error(error).to_string(),
            error: error.to_string(),
        },
        None => ProbeStageCompletion::Succeeded,
    };
    store.finish_probe_stage(&outro_key, job_id, &completion, now_ms)?;
    Ok(())
}

pub(super) fn classify_fingerprint_error(error: &str) -> &'static str {
    if error.contains("403") {
        "http_403"
    } else if error.contains("404") {
        "http_404"
    } else if error.contains("empty") {
        "empty_audio"
    } else {
        "network"
    }
}

#[derive(Clone, Debug)]
pub enum FingerprintRunOutcome {
    Complete,
    Retry,
    Partial { error_kind: String, error: String },
    Failed { error_kind: String, error: String },
    Cancelled { reason: String },
}

pub fn stage_completion_from_outcome(
    stage: ProbeStage,
    outcome: &FingerprintRunOutcome,
) -> Option<ProbeStageCompletion> {
    Some(match outcome {
        FingerprintRunOutcome::Retry => return None,
        FingerprintRunOutcome::Complete => ProbeStageCompletion::Succeeded,
        FingerprintRunOutcome::Cancelled { reason } => ProbeStageCompletion::Cancelled {
            reason: reason.clone(),
        },
        FingerprintRunOutcome::Failed { error_kind, error } => ProbeStageCompletion::Failed {
            error_kind: error_kind.clone(),
            error: error.clone(),
        },
        FingerprintRunOutcome::Partial { error_kind, error } => match stage {
            ProbeStage::Intro => ProbeStageCompletion::Succeeded,
            ProbeStage::Outro => ProbeStageCompletion::Failed {
                error_kind: error_kind.clone(),
                error: error.clone(),
            },
            ProbeStage::Metadata => ProbeStageCompletion::Succeeded,
        },
    })
}

/// Preserve partial intro captures while reporting the missing outro separately.
pub(super) fn capture_outcome(
    unit: &super::ProbeUnit,
    error: Option<String>,
    require_complete_markers: bool,
) -> FingerprintRunOutcome {
    match error {
        Some(error) => {
            tracing::warn!(
                %error,
                ledger_id = %unit.row.file.file_id,
                media_id = %unit.row.file.media_id,
                season = ?super::single_season(&unit.row),
                episode = ?super::single_episode(&unit.row),
                require_complete_markers,
                "【声纹】片尾指纹暂不可用，保留片头缓存并允许下次只重试片尾"
            );
            let error_kind = classify_fingerprint_error(&error).to_string();
            if require_complete_markers {
                FingerprintRunOutcome::Failed { error_kind, error }
            } else {
                FingerprintRunOutcome::Partial { error_kind, error }
            }
        }
        None => FingerprintRunOutcome::Complete,
    }
}
