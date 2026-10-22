use store::{ProbeStage, ProbeStageCompletion, ProbeStageKey};

use super::ProbeManager;

pub(super) fn record_fingerprint_stages(
    mgr: &ProbeManager,
    ledger_id: &str,
    job_id: &str,
    cache_key: &str,
    outro_error: Option<&str>,
    now_ms: i64,
) {
    let intro_key = ProbeStageKey {
        ledger_id: ledger_id.to_string(),
        context_key: cache_key.to_string(),
        stage: ProbeStage::Intro,
    };
    let store = mgr.store.lock();
    // The attempt's context becomes the file's current context; older contexts
    // retire in the same short transaction so they cannot occupy the retry window.
    if let Err(error) = store.adopt_probe_context(ledger_id, cache_key) {
        tracing::warn!(%error, file_id = %ledger_id, context = %cache_key, "声纹上下文收敛失败");
    }
    claim_stage(&store, &intro_key, job_id, now_ms);
    finish_stage(
        &store,
        &intro_key,
        job_id,
        &ProbeStageCompletion::Succeeded,
        now_ms,
    );

    let outro_key = ProbeStageKey {
        ledger_id: ledger_id.to_string(),
        context_key: cache_key.to_string(),
        stage: ProbeStage::Outro,
    };
    claim_stage(&store, &outro_key, job_id, now_ms);
    let completion = match outro_error {
        Some(error) => ProbeStageCompletion::Failed {
            error_kind: classify_fingerprint_error(error).to_string(),
            error: error.to_string(),
        },
        None => ProbeStageCompletion::Succeeded,
    };
    finish_stage(&store, &outro_key, job_id, &completion, now_ms);
}

/// Stage bookkeeping is retry state, not a probe result: a store failure is reported, not
/// swallowed, and never aborts the voiceprint run it describes.
fn claim_stage(store: &crate::Store, key: &ProbeStageKey, job_id: &str, now_ms: i64) {
    if let Err(error) = store.claim_probe_stage(key, job_id, now_ms) {
        tracing::warn!(
            %error,
            file_id = %key.ledger_id,
            stage = key.stage.as_str(),
            "声纹阶段开始状态写入失败"
        );
    }
}

fn finish_stage(
    store: &crate::Store,
    key: &ProbeStageKey,
    job_id: &str,
    completion: &ProbeStageCompletion,
    now_ms: i64,
) {
    if let Err(error) = store.finish_probe_stage(key, job_id, completion, now_ms) {
        tracing::warn!(
            %error,
            file_id = %key.ledger_id,
            stage = key.stage.as_str(),
            "声纹阶段终态写入失败"
        );
    }
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
    Partial { error_kind: String, error: String },
    Failed { error_kind: String, error: String },
    Cancelled { reason: String },
}

pub fn stage_completion_from_outcome(
    stage: ProbeStage,
    outcome: &FingerprintRunOutcome,
) -> ProbeStageCompletion {
    match outcome {
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
    }
}
