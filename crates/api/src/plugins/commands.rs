use domain::PluginOperationId;
use store::StoreError;
use store::plugins::{
    AcceptPluginOperation, AcceptedPluginOperation, PluginCommandClaim, PluginCommandOutcome,
    PluginEffectObservation, PluginError, PluginOperation, PluginOperationState,
    PluginPendingCommand,
};

use super::effect::{ExternalEffectStep, PluginEffectContext, sanitize_phase_token};
use super::errors::PluginServiceError;
use super::service::SubmitPluginCommand;
use crate::management::ApiState;

pub fn submit_command(
    state: &ApiState,
    req: SubmitPluginCommand,
) -> Result<AcceptedPluginOperation, PluginServiceError> {
    submit_command_with_deps(&state.store, &state.jobs, state.plugins.effects(), req)
}

pub(super) fn submit_command_with_deps(
    store: &std::sync::Arc<store::Store>,
    jobs: &std::sync::Arc<crate::JobsWriter>,
    effects: &std::sync::Arc<dyn super::effect::PluginEffectAdapter>,
    req: SubmitPluginCommand,
) -> Result<AcceptedPluginOperation, PluginServiceError> {
    if !req.role.allows_command_submit() {
        return Err(PluginServiceError::WrongContext);
    }
    let command_authority =
        store.authorize_plugin_command(&req.authority, &req.kind, &req.targets)?;
    if !effects.supports_kind(&req.kind) {
        return Err(PluginServiceError::UnsupportedCommand(req.kind));
    }
    let expected_revision = req
        .expected_revision
        .unwrap_or_else(|| req.authority.revision());
    let accept_input = AcceptPluginOperation {
        plugin_id: Some(req.authority.plugin_id().to_string()),
        subject: req.subject,
        idempotency_key: req.idempotency_key,
        request: req.request,
        kind: req.kind,
        expected_revision: Some(expected_revision),
        expected_generation: Some(req.authority.generation()),
        cause: req.cause,
    };
    let accepted = store.accept_plugin_command(&command_authority, &accept_input, &req.payload)?;
    if matches!(
        accepted.operation.state,
        PluginOperationState::Queued | PluginOperationState::Deferred
    ) && let Err(error) = project_command_job_with_jobs(jobs, &accepted.operation)
    {
        if matches!(error, PluginServiceError::Store(StoreError::WriteBusy)) {
            tracing::warn!(
                operation_id = %accepted.operation.operation_id,
                "Jobs writer busy during foreground plugin command projection; deferred to reconciliation"
            );
        } else {
            return Err(error);
        }
    }
    Ok(accepted)
}

pub(super) fn project_command_job(
    state: &ApiState,
    op: &PluginOperation,
) -> Result<(), PluginServiceError> {
    project_command_job_with_jobs(&state.jobs, op)
}

fn project_command_job_with_jobs(
    jobs: &std::sync::Arc<crate::JobsWriter>,
    op: &PluginOperation,
) -> Result<(), PluginServiceError> {
    project_operation_job_with_jobs(
        jobs,
        op,
        jobs::JobKind::PluginCommand,
        "cmd",
        op.next_run_at,
    )
}

pub(super) fn project_reconcile_job(
    state: &ApiState,
    op: &PluginOperation,
) -> Result<(), PluginServiceError> {
    project_operation_job_with_jobs(
        &state.jobs,
        op,
        jobs::JobKind::PluginReconcile,
        "reconcile",
        None,
    )
}

fn project_operation_job_with_jobs(
    jobs: &std::sync::Arc<crate::JobsWriter>,
    op: &PluginOperation,
    kind: jobs::JobKind,
    prefix: &str,
    run_after: Option<i64>,
) -> Result<(), PluginServiceError> {
    let op_id = PluginOperationId::from_uuid(op.operation_id);
    let sequence = u32::try_from(op.revision)
        .map_err(|_| jobs::JobError::Identity("plugin operation revision out of range".into()))?;
    let concurrency_key = Some(format!("{prefix}:{}", op.operation_id));
    let spec = jobs::NewPluginOperationJob {
        operation_id: op_id,
        kind,
        sequence,
        generation: op.generation,
        request_key: format!("{prefix}:{}:rev:{}", op.operation_id, op.revision),
        payload: serde_json::json!({ "operation_id": op_id }).to_string(),
        run_after: run_after.unwrap_or(0),
        concurrency_key,
        timeout_secs: Some(60),
    };
    let reservation = jobs.reserve()?;
    let queue = reservation.acquire()?;
    queue.project_plugin_operation(spec)?;
    Ok(())
}

pub fn run_command_job(state: &ApiState, job: &jobs::Job) -> Result<(), String> {
    let jobs::JobScope::PluginOperation(op_id) = job.scope else {
        return Err("PluginCommand requires PluginOperation scope".into());
    };
    let now = job.started_at.unwrap_or_else(crate::job_loop::unix_now);
    let pending = state
        .store
        .plugin_command_status(op_id.as_uuid())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("plugin operation {op_id} not found"))?;
    if needs_reconcile(&pending.operation) {
        project_reconcile_job(state, &pending.operation).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !matches!(
        pending.operation.state,
        PluginOperationState::Queued | PluginOperationState::Deferred
    ) {
        return Ok(());
    }
    let Some(claim) = claim_for_execution(state, op_id, &pending, now)? else {
        return Ok(());
    };
    if let Some(hook) = state.plugins.pre_effect_hook() {
        hook(op_id);
    }
    let Some(claimed) = inspect_or_reject_stale(state, &claim)? else {
        return Ok(());
    };
    let ctx = PluginEffectContext {
        operation_id: op_id,
        plugin_id: claimed.operation.plugin_id.as_deref(),
        kind: &claimed.operation.kind,
        attempt: claim.attempt(),
        generation: claim.generation(),
        subject: &claimed.subject,
        canonical_method: &claimed.canonical_method,
        targets: &claimed.targets,
        cause: &claimed.cause,
        payload: claimed.payload.as_ref().map(|p| p.as_bytes()),
        now,
    };
    let step = match state.plugins.effects().execute(&ctx) {
        ExternalEffectStep::Rejected { ref phase } if phase == "delegate-site-maintenance" => {
            crate::site_maintenance::execute_effect_step(state, &ctx)
        }
        ExternalEffectStep::Rejected { ref phase }
            if phase == "delegate-downloader-add" || phase == "delegate-downloader-command" =>
        {
            crate::download_service::execute_plugin_command_step(state, &ctx)
        }
        ExternalEffectStep::Rejected { ref phase } if phase == "delegate-boost-run" => {
            crate::plugins::boost::execute_boost_run_command(state, &ctx)
        }
        ExternalEffectStep::Rejected { ref phase } if phase == "delegate-indexer-search" => {
            super::ui_indexer::execute_indexer_search_command(state, &ctx)
        }
        other => other,
    };
    persist_effect_step(state, &claim, step)
}

pub(super) fn needs_reconcile(op: &PluginOperation) -> bool {
    op.state == PluginOperationState::Unknown
        || (op.effect == PluginEffectObservation::Confirmed && !op.state.is_terminal())
}

fn ensure_reconcile_by_id(state: &ApiState, id: uuid::Uuid) -> Result<(), String> {
    let Some(pending) = state
        .store
        .plugin_command_status(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if needs_reconcile(&pending.operation) {
        project_reconcile_job(state, &pending.operation).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn claim_for_execution(
    state: &ApiState,
    op_id: PluginOperationId,
    pending: &PluginPendingCommand,
    now: i64,
) -> Result<Option<PluginCommandClaim>, String> {
    match state.store.claim_plugin_command(
        op_id.as_uuid(),
        pending.operation.revision,
        pending.operation.generation,
        pending.attempt,
        now,
    ) {
        Ok(claim) => Ok(Some(claim)),
        Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
            match state.store.reject_stale_plugin_command(
                op_id.as_uuid(),
                pending.operation.revision,
                pending.operation.generation,
                pending.attempt,
                now,
            ) {
                Ok(_) | Err(StoreError::Plugin(PluginError::Conflict)) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
        Err(StoreError::Plugin(
            PluginError::UnknownEffect | PluginError::EffectDispositionRequired,
        )) => {
            ensure_reconcile_by_id(state, op_id.as_uuid())?;
            Ok(None)
        }
        Err(StoreError::Plugin(PluginError::Conflict)) => {
            let still_due = state
                .store
                .plugin_command_status(op_id.as_uuid())
                .map_err(|e| e.to_string())?
                .is_some_and(|p| {
                    matches!(
                        p.operation.state,
                        PluginOperationState::Queued | PluginOperationState::Deferred
                    )
                });
            if still_due {
                Err("plugin command claim conflict".into())
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

fn rejected_outcome(phase: &str) -> PluginCommandOutcome {
    PluginCommandOutcome {
        state: PluginOperationState::Rejected,
        phase: phase.into(),
        next_run_at: None,
        result: None,
        effect: PluginEffectObservation::NoEffect,
    }
}

fn inspect_or_reject_stale(
    state: &ApiState,
    claim: &PluginCommandClaim,
) -> Result<Option<store::plugins::PluginClaimedCommand>, String> {
    match state.store.inspect_claimed_plugin_command(claim) {
        Ok(claimed) => Ok(Some(claimed)),
        Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
            match state
                .store
                .record_plugin_command_outcome(claim, &rejected_outcome("authority-rejected"))
            {
                Ok(_) => Ok(None),
                Err(StoreError::Plugin(PluginError::Conflict)) => {
                    ensure_reconcile_by_id(state, claim.operation_id())?;
                    Ok(None)
                }
                Err(error) => Err(error.to_string()),
            }
        }
        Err(StoreError::Plugin(PluginError::Conflict)) => {
            ensure_reconcile_by_id(state, claim.operation_id())?;
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn map_step_outcome(step: ExternalEffectStep) -> PluginCommandOutcome {
    match step {
        ExternalEffectStep::Succeeded { result } => PluginCommandOutcome {
            state: PluginOperationState::Succeeded,
            phase: "completed".into(),
            next_run_at: None,
            result,
            effect: PluginEffectObservation::Confirmed,
        },
        ExternalEffectStep::Deferred { next_run_at, phase } => PluginCommandOutcome {
            state: PluginOperationState::Deferred,
            phase: sanitize_phase_token(&phase, "deferred"),
            next_run_at: Some(next_run_at),
            result: None,
            effect: PluginEffectObservation::NoEffect,
        },
        ExternalEffectStep::Rejected { phase } => {
            rejected_outcome(&sanitize_phase_token(&phase, "rejected"))
        }
        ExternalEffectStep::Failed { phase } => PluginCommandOutcome {
            state: PluginOperationState::Failed,
            phase: sanitize_phase_token(&phase, "failed"),
            next_run_at: None,
            result: None,
            effect: PluginEffectObservation::NoEffect,
        },
        ExternalEffectStep::Unknown { phase } => PluginCommandOutcome {
            state: PluginOperationState::Unknown,
            phase: sanitize_phase_token(&phase, "unknown"),
            next_run_at: None,
            result: None,
            effect: PluginEffectObservation::Unknown,
        },
    }
}

fn persist_effect_step(
    state: &ApiState,
    claim: &PluginCommandClaim,
    step: ExternalEffectStep,
) -> Result<(), String> {
    let outcome = map_step_outcome(step);
    let has_effect = outcome.effect != PluginEffectObservation::NoEffect;
    match state.store.record_plugin_command_outcome(claim, &outcome) {
        Ok(updated) => {
            if updated.state == PluginOperationState::Deferred
                && let Err(error) = project_command_job(state, &updated)
            {
                tracing::warn!(operation_id = %updated.operation_id, %error, "Deferred command projection will retry on reconciliation");
            }
            if needs_reconcile(&updated) {
                project_reconcile_job(state, &updated).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {
            if has_effect {
                let _ = state.store.record_plugin_command_interruption(
                    claim.operation_id(),
                    claim.revision(),
                    claim.generation(),
                    claim.attempt(),
                );
                ensure_reconcile_by_id(state, claim.operation_id())?;
            } else {
                let _ = state
                    .store
                    .record_plugin_command_outcome(claim, &rejected_outcome("authority-rejected"));
            }
            Ok(())
        }
        Err(StoreError::Plugin(PluginError::Conflict)) => {
            ensure_reconcile_by_id(state, claim.operation_id())?;
            Ok(())
        }
        Err(StoreError::Plugin(PluginError::Invalid(msg))) => {
            tracing::error!(
                operation_id = %claim.operation_id(),
                phase = %outcome.phase,
                error = msg,
                "invalid plugin command outcome; forcing fallback settlement"
            );
            if has_effect {
                let _ = state.store.record_plugin_command_interruption(
                    claim.operation_id(),
                    claim.revision(),
                    claim.generation(),
                    claim.attempt(),
                );
                ensure_reconcile_by_id(state, claim.operation_id())?;
            } else {
                let fallback_state = if outcome.state == PluginOperationState::Rejected {
                    PluginOperationState::Rejected
                } else {
                    PluginOperationState::Failed
                };
                let _ = state.store.record_plugin_command_outcome(
                    claim,
                    &PluginCommandOutcome {
                        state: fallback_state,
                        phase: "invalid-outcome".into(),
                        next_run_at: None,
                        result: None,
                        effect: PluginEffectObservation::NoEffect,
                    },
                );
            }
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_valid_phase(phase: &str) -> bool {
        !phase.is_empty()
            && phase.len() <= 128
            && phase
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
    }

    #[test]
    fn map_step_outcome_sanitizes_all_non_succeeded_phases() {
        let long = format!("bad `{}` {}", "z".repeat(150), "中文");
        for (step, fallback) in [
            (
                ExternalEffectStep::Deferred {
                    next_run_at: 10,
                    phase: long.clone(),
                },
                "deferred",
            ),
            (
                ExternalEffectStep::Rejected {
                    phase: "bad `tag` 中文".into(),
                },
                "rejected",
            ),
            (ExternalEffectStep::Failed { phase: "".into() }, "failed"),
            (
                ExternalEffectStep::Unknown {
                    phase: "中文 only".into(),
                },
                "unknown",
            ),
        ] {
            let outcome = map_step_outcome(step);
            assert!(
                is_valid_phase(&outcome.phase),
                "invalid phase {:?} (fallback {fallback})",
                outcome.phase
            );
        }
    }
}
