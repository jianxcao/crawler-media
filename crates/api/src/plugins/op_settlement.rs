use domain::UserId;
use store::Store;
use store::plugins::{
    CanonicalPluginRequest, PluginCommandClaim, PluginCommandOutcome, PluginEffectObservation,
    PluginLifecycle, PluginOperationState, PluginSubject,
};
use uuid::Uuid;

use super::reauth::now_unix;
use super::service::{run_app_read, run_blocking, run_foreground_app_tx};
use super::types::{OperationAdminView, PluginServiceError, format_rfc3339};
use crate::management::ApiState;

pub(crate) fn op_to_admin_view(
    op: &store::plugins::PluginOperation,
    job_id: Option<String>,
    result: Option<serde_json::Value>,
) -> OperationAdminView {
    let progress = match op.state {
        PluginOperationState::Succeeded
        | PluginOperationState::Rejected
        | PluginOperationState::Failed
        | PluginOperationState::Cancelled => 100,
        PluginOperationState::Running => 50,
        _ => 0,
    };
    let blocked_reason = (op.state == PluginOperationState::Blocked).then(|| op.phase.clone());
    let error = matches!(
        op.state,
        PluginOperationState::Failed | PluginOperationState::Rejected
    )
    .then(|| op.phase.clone());
    let retry_after_ms = (!op.state.is_terminal()).then_some(500);
    OperationAdminView {
        operation_id: op.operation_id,
        plugin_id: op.plugin_id.clone(),
        kind: op.kind.clone(),
        state: op.state,
        phase: op.phase.clone(),
        revision: op.revision,
        generation: op.generation,
        job_id,
        progress,
        blocked_reason,
        error,
        result,
        next_run_at: op.next_run_at.map(format_rfc3339),
        retry_after_ms,
        created_at: format_rfc3339(op.created_at),
        updated_at: format_rfc3339(op.updated_at),
    }
}

pub(crate) fn load_operation_admin_view(
    store: &Store,
    op_id: Uuid,
) -> Result<OperationAdminView, PluginServiceError> {
    let admin = store.get_plugin_operation_admin(op_id)?.ok_or_else(|| {
        PluginServiceError::not_found("plugin.operation_missing", "operation not found")
    })?;
    let job_id = admin
        .result_json
        .as_ref()
        .and_then(|v| v.get("job_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(op_to_admin_view(
        &admin.operation,
        job_id,
        admin.result_json,
    ))
}

pub(crate) async fn check_management_replay(
    state: &ApiState,
    actor: UserId,
    plugin_id: Option<&str>,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
    kind: &str,
) -> Result<Option<OperationAdminView>, PluginServiceError> {
    super::idempotency::validate_idempotency_input(idempotency_key, request)?;
    let pid = plugin_id.map(str::to_string);
    let key = idempotency_key.to_string();
    let req = request.clone();
    let k = kind.to_string();
    run_app_read(state, move |store| {
        let effective_kind = match pid.as_deref() {
            Some(p) => super::admin_command_handles::normalize_admin_command_kind(store, p, &k),
            None => k,
        };
        let Some(op) = store.get_plugin_operation_by_idempotency(
            &PluginSubject::User(actor),
            pid.as_deref(),
            &key,
            &req,
            &effective_kind,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(load_operation_admin_view(store, op.operation_id)?))
    })
    .await
}

pub(crate) async fn check_management_replay_after_write(
    state: &ApiState,
    actor: UserId,
    plugin_id: Option<&str>,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
    kind: &str,
) -> Result<Option<OperationAdminView>, PluginServiceError> {
    super::idempotency::wait_app_write_idle(state).await;
    check_management_replay(state, actor, plugin_id, idempotency_key, request, kind).await
}

pub(crate) fn complete_non_staged_op(
    store: &Store,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    complete_non_staged_op_with_result(store, claim, succeeded, phase, None)
}

pub(crate) fn complete_non_staged_app_op(
    app: &store::App<'_>,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    complete_non_staged_app_op_with_result(app, claim, succeeded, phase, None)
}

pub(crate) fn complete_non_staged_app_op_with_result(
    app: &store::App<'_>,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
    result: Option<Vec<u8>>,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    let outcome = PluginCommandOutcome {
        state: if succeeded {
            PluginOperationState::Succeeded
        } else {
            PluginOperationState::Failed
        },
        phase: phase.into(),
        next_run_at: None,
        result,
        effect: PluginEffectObservation::NoEffect,
    };
    Ok(app.record_plugin_command_outcome(claim, &outcome)?)
}

pub(crate) fn complete_foreground_non_staged_op(
    state: &ApiState,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    complete_foreground_non_staged_op_with_result(state, claim, succeeded, phase, None)
}

pub(crate) fn complete_foreground_non_staged_op_with_result(
    state: &ApiState,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
    result: Option<Vec<u8>>,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    run_foreground_app_tx(state, true, |app| {
        complete_non_staged_app_op_with_result(app, claim, succeeded, phase, result)
    })
}

pub(crate) fn complete_non_staged_op_with_result(
    store: &Store,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
    result: Option<Vec<u8>>,
) -> Result<store::plugins::PluginOperation, PluginServiceError> {
    let outcome = PluginCommandOutcome {
        state: if succeeded {
            PluginOperationState::Succeeded
        } else {
            PluginOperationState::Failed
        },
        phase: phase.into(),
        next_run_at: None,
        result,
        effect: PluginEffectObservation::NoEffect,
    };
    Ok(store.record_plugin_command_outcome(claim, &outcome)?)
}

pub(crate) fn settle_drain_failure(
    state: &ApiState,
    claim: &PluginCommandClaim,
    err: &PluginServiceError,
) {
    if super::recovery::is_transient_or_admission_err(err) {
        return;
    }
    let phase = match err {
        PluginServiceError::Conflict {
            code: "plugin.not_drained",
            ..
        } => "not-drained",
        _ => "drain-failed",
    };
    let _ = complete_foreground_non_staged_op(state, claim, false, phase);
}

pub(crate) fn settle_failed_staged_operation_mode(
    state: &ApiState,
    op_id: Uuid,
    phase: &str,
    foreground: bool,
) -> Result<(), PluginServiceError> {
    let store = &state.store;
    let Some(admin) = store.get_plugin_operation_admin(op_id)? else {
        return Ok(());
    };
    if admin.operation.state.is_terminal() {
        return Ok(());
    }
    if let Some(intent) = &admin.intent
        && let Some(p) = store.get_plugin(&intent.target_plugin_id)?
        && p.active_operation_id == Some(op_id)
        && matches!(
            p.lifecycle,
            PluginLifecycle::Enabling | PluginLifecycle::Draining
        )
    {
        return Ok(());
    }
    if admin.operation.state == PluginOperationState::Running
        && (admin.operation.phase == "resuming"
            || super::idempotency::has_active_resume_claim(store, op_id))
    {
        return Ok(());
    }
    if admin.operation.state == PluginOperationState::Unknown {
        if admin.operation.phase == "cancel-requested" && admin.intent.is_some() {
            return super::recovery::reconcile_cancel_requested_staged_op_mode(
                state, &admin, foreground,
            );
        }
        return run_foreground_app_tx(state, foreground, |app| {
            let _ =
                app.reconcile_plugin_command(&store::plugins::PluginReconciliationObservation {
                    operation_id: op_id,
                    revision: admin.operation.revision,
                    generation: admin.operation.generation,
                    attempt: admin.attempt,
                    outcome: store::plugins::PluginReconciliationOutcome::Failed,
                });
            super::idempotency::settle_resume_idempotency_in_app(app, op_id, None)?;
            Ok(())
        });
    }
    run_foreground_app_tx(state, foreground, |app| {
        let now = now_unix();
        let claim = match admin.operation.state {
            PluginOperationState::Queued | PluginOperationState::Deferred => app
                .claim_staged_plugin_operation(op_id, admin.operation.revision, admin.attempt, now),
            PluginOperationState::Running | PluginOperationState::Blocked => app
                .recover_staged_plugin_operation(
                    op_id,
                    admin.operation.revision,
                    admin.attempt,
                    "failing",
                    now,
                ),
            _ => return Ok(()),
        };
        if let Ok(fresh_claim) = claim {
            let _ = app.complete_staged_plugin_operation(
                &fresh_claim,
                PluginOperationState::Failed,
                phase,
                None,
                Some(phase),
                now,
            );
            super::idempotency::settle_resume_idempotency_in_app(app, op_id, None)?;
        }
        Ok(())
    })
}

#[doc(hidden)]
pub async fn fail_staged_operation(state: &ApiState, op_id: Uuid, phase: &'static str) {
    let s = state.clone();
    let _ = run_blocking(move || settle_failed_staged_operation_mode(&s, op_id, phase, true)).await;
}
