use store::StoreError;
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginError, PluginLifecycle,
    PluginOperationState, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    OpBegin, is_consent_stale_err, is_reauth_invalid_err, load_app_operation_admin_view,
};
use super::lifecycle_ops::{check_management_replay, check_management_replay_after_write};
use super::reauth::{ConsentPayload, digest_scopes, now_unix};
use super::service::{PluginService, run_app_read};
use super::types::{
    ExecuteStateResetInput, OperationAdminView, PluginServiceError, PrepareStateResetInput,
    ReauthPurpose, StateResetPreparationView,
};
use crate::management::ApiState;

impl PluginService {
    pub async fn prepare_state_reset(
        &self,
        state: &ApiState,
        actor_user_id: domain::UserId,
        plugin_id: &str,
        input: PrepareStateResetInput,
    ) -> Result<StateResetPreparationView, PluginServiceError> {
        let pid = plugin_id.to_string();
        let scopes = input.security_scopes.clone();
        let (plugin, preview) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let scope_opt = (!scopes.is_empty()).then_some(scopes.as_slice());
            let preview = store.preview_plugin_state_reset(&pid, scope_opt)?;
            Ok((p, preview))
        })
        .await?;

        let scopes_digest = digest_scopes(&input.security_scopes)?;
        let preparation_id = Uuid::new_v4();
        let (consent_token, expires_at) = self.consent().mint(
            actor_user_id,
            preparation_id,
            ConsentPayload::StateReset {
                plugin_id: plugin_id.to_string(),
                base_revision: plugin.revision,
                state_revision: preview.state_revision,
                scopes_digest,
            },
        );
        Ok(StateResetPreparationView {
            preparation_id,
            plugin_id: plugin_id.to_string(),
            base_revision: plugin.revision,
            kv_entries: preview.kv_entries,
            blob_entries: preview.blob_entries,
            kv_bytes: preview.kv_bytes,
            blob_bytes: preview.blob_bytes,
            state_revision: preview.state_revision,
            consent_token,
            expires_at,
        })
    }

    pub async fn execute_state_reset(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: ExecuteStateResetInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "state-reset",
        )
        .await?
        {
            return Ok(replayed);
        }

        if let Err(err) = self
            .peek_reauth_token(
                state,
                input.actor_user_id,
                ReauthPurpose::PluginStateReset,
                &input.reauth_token,
            )
            .await
        {
            if is_reauth_invalid_err(&err)
                && let Some(replayed) = check_management_replay_after_write(
                    state,
                    input.actor_user_id,
                    Some(plugin_id),
                    &input.idempotency_key,
                    &input.request,
                    "state-reset",
                )
                .await?
            {
                return Ok(replayed);
            }
            return Err(err);
        }

        let scopes_digest = digest_scopes(&input.security_scopes)?;
        let state_revision = match peek_state_reset_consent(self, plugin_id, &input, scopes_digest)
        {
            Ok(sr) => sr,
            Err(err) => {
                if is_consent_stale_err(&err)
                    && let Some(replayed) = check_management_replay_after_write(
                        state,
                        input.actor_user_id,
                        Some(plugin_id),
                        &input.idempotency_key,
                        &input.request,
                        "state-reset",
                    )
                    .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        if let Err(err) = precheck_state_reset(state, plugin_id, &input, state_revision).await {
            if matches!(
                err,
                PluginServiceError::Conflict { .. }
                    | PluginServiceError::Store(StoreError::Plugin(
                        PluginError::Conflict
                            | PluginError::NotDrained
                            | PluginError::OperationActive(_)
                    ))
            ) && let Some(replayed) = check_management_replay_after_write(
                state,
                input.actor_user_id,
                Some(plugin_id),
                &input.idempotency_key,
                &input.request,
                "state-reset",
            )
            .await?
            {
                return Ok(replayed);
            }
            return Err(err);
        }

        execute_state_reset_write(self, state, plugin_id, input, state_revision, scopes_digest)
            .await
    }
}

fn match_state_reset_consent(
    p: &ConsentPayload,
    plugin_id: &str,
    expected_revision: i64,
    scopes_digest: [u8; 32],
) -> Result<i64, PluginServiceError> {
    match p {
        ConsentPayload::StateReset {
            plugin_id: pid,
            base_revision,
            state_revision,
            scopes_digest: sd,
        } if pid == plugin_id && *base_revision == expected_revision && *sd == scopes_digest => {
            Ok(*state_revision)
        }
        _ => Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "state reset consent_token mismatch",
        )),
    }
}

fn peek_state_reset_consent(
    service: &PluginService,
    plugin_id: &str,
    input: &ExecuteStateResetInput,
    scopes_digest: [u8; 32],
) -> Result<i64, PluginServiceError> {
    service
        .consent()
        .peek(input.actor_user_id, None, &input.consent_token, |p| {
            match_state_reset_consent(p, plugin_id, input.expected_revision, scopes_digest)
        })
}

async fn precheck_state_reset(
    state: &ApiState,
    plugin_id: &str,
    input: &ExecuteStateResetInput,
    state_revision: i64,
) -> Result<(), PluginServiceError> {
    let pid = plugin_id.to_string();
    let expected_rev = input.expected_revision;
    let scopes = input.security_scopes.clone();
    run_app_read(state, move |store| {
        let p = store
            .get_plugin(&pid)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        if let Some(active) = p.active_operation_id {
            return Err(PluginServiceError::Store(
                PluginError::OperationActive(active).into(),
            ));
        }
        if p.revision != expected_rev || p.revision == i64::MAX {
            return Err(PluginServiceError::Store(PluginError::Conflict.into()));
        }
        if !matches!(
            p.lifecycle,
            PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
        ) || p.desired_enabled
            || store.has_unresolved_plugin_operations(&pid)?
        {
            return Err(PluginServiceError::Store(PluginError::NotDrained.into()));
        }
        let scope_opt = (!scopes.is_empty()).then_some(scopes.as_slice());
        let current_preview = store.preview_plugin_state_reset(&pid, scope_opt)?;
        if current_preview.state_revision != state_revision {
            return Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "plugin state changed since preparation",
            ));
        }
        Ok(())
    })
    .await
}

struct StateResetTxParams {
    pid: String,
    actor: domain::UserId,
    expected_rev: i64,
    scopes: Vec<store::plugins::PluginSecurityScope>,
    state_revision: i64,
    scopes_digest: [u8; 32],
    consent: std::sync::Arc<super::reauth::ConsentStore>,
    consent_tok: String,
    reauth: std::sync::Arc<super::reauth::ReauthService>,
    reauth_tok: String,
    idem: String,
    req: store::plugins::CanonicalPluginRequest,
}

fn commit_state_reset_in_app(
    app: &store::App<'_>,
    p: StateResetTxParams,
) -> Result<OpBegin<OperationAdminView>, PluginServiceError> {
    if let Some(op) = app.get_plugin_operation_by_idempotency(
        &PluginSubject::User(p.actor),
        Some(&p.pid),
        &p.idem,
        &p.req,
        "state-reset",
    )? {
        return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
            app,
            op.operation_id,
        )?)));
    }
    let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
        subject: PluginSubject::User(p.actor),
        idempotency_key: p.idem,
        request: p.req,
        op_kind: StagedPluginOpKind::StateReset,
        expected_revision: p.expected_rev,
        intent: StagedPluginIntent {
            target_plugin_id: p.pid.clone(),
            package_hash: None,
            version: None,
            artifact_path: None,
            state_policy: Some("reset".into()),
            target_config_digest: None,
            grants: Vec::new(),
        },
        cause: PluginCommandCause::root(format!("state-reset:{}", p.pid)),
    })?;
    let op_id = accepted.operation.operation_id;
    if accepted.replayed {
        return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
            app, op_id,
        )?)));
    }
    let scope_opt = (!p.scopes.is_empty()).then_some(p.scopes.as_slice());
    let current_preview = app.preview_plugin_state_reset(&p.pid, scope_opt)?;
    if current_preview.state_revision != p.state_revision {
        return Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "plugin state changed since preparation",
        ));
    }
    let claim =
        app.claim_staged_plugin_operation(op_id, accepted.operation.revision, 0, now_unix())?;
    let reset = app.reset_plugin_state(&p.pid, p.expected_rev, Some(op_id), scope_opt)?;
    p.consent.peek(p.actor, None, &p.consent_tok, |payload| {
        match_state_reset_consent(payload, &p.pid, p.expected_rev, p.scopes_digest).map(|_| ())
    })?;
    p.reauth
        .consume_in_app_tx(app, p.actor, ReauthPurpose::PluginStateReset, &p.reauth_tok)?;
    let _ = p
        .consent
        .peek_consume(p.actor, None, &p.consent_tok, |payload| {
            match_state_reset_consent(payload, &p.pid, p.expected_rev, p.scopes_digest).map(|_| ())
        })?;
    let result = serde_json::json!({
        "plugin_id": p.pid,
        "deleted_kv_rows": reset.deleted_kv_rows,
        "deleted_blob_rows": reset.deleted_blob_rows,
    });
    app.complete_staged_plugin_operation(
        &claim,
        PluginOperationState::Succeeded,
        "reset",
        Some(&result),
        None,
        now_unix(),
    )?;
    Ok(OpBegin::Proceed(load_app_operation_admin_view(app, op_id)?))
}

fn is_replayable_state_reset_err(err: &PluginServiceError) -> bool {
    is_consent_stale_err(err)
        || is_reauth_invalid_err(err)
        || matches!(
            err,
            PluginServiceError::Conflict { .. }
                | PluginServiceError::Store(StoreError::Plugin(
                    PluginError::Conflict
                        | PluginError::NotDrained
                        | PluginError::OperationActive(_)
                ))
        )
}

async fn execute_state_reset_write(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    input: ExecuteStateResetInput,
    state_revision: i64,
    scopes_digest: [u8; 32],
) -> Result<OperationAdminView, PluginServiceError> {
    let actor = input.actor_user_id;
    let idem_retry = input.idempotency_key.clone();
    let req_retry = input.request.clone();
    let params = StateResetTxParams {
        pid: plugin_id.to_string(),
        actor,
        expected_rev: input.expected_revision,
        scopes: input.security_scopes,
        state_revision,
        scopes_digest,
        consent: service.consent().clone(),
        consent_tok: input.consent_token,
        reauth: service.reauth().clone(),
        reauth_tok: input.reauth_token,
        idem: input.idempotency_key,
        req: input.request,
    };
    let write_res =
        run_app_write_tx(state, move |app| commit_state_reset_in_app(app, params)).await;
    match write_res {
        Ok(OpBegin::Replayed(v)) => Ok(*v),
        Ok(OpBegin::Proceed(v)) => Ok(v),
        Err(err) => {
            if is_replayable_state_reset_err(&err)
                && let Some(replayed) = check_management_replay_after_write(
                    state,
                    actor,
                    Some(plugin_id),
                    &idem_retry,
                    &req_retry,
                    "state-reset",
                )
                .await?
            {
                return Ok(replayed);
            }
            Err(err)
        }
    }
}
