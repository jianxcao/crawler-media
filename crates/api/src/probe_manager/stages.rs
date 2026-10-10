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
    let _ = store.claim_probe_stage(&intro_key, job_id, now_ms);
    let _ = store.finish_probe_stage(
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
    let _ = store.claim_probe_stage(&outro_key, job_id, now_ms);
    let completion = match outro_error {
        Some(error) => ProbeStageCompletion::Failed {
            error_kind: classify_fingerprint_error(error).to_string(),
            error: error.to_string(),
        },
        None => ProbeStageCompletion::Succeeded,
    };
    let _ = store.finish_probe_stage(&outro_key, job_id, &completion, now_ms);
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
