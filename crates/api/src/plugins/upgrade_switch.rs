use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginError, PluginGrant, PluginLifecycle, PluginRecord, PluginStatePolicy, PluginSubject,
    PluginUpgradeConfig, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    OpBegin, is_consent_stale_err, is_reauth_invalid_err, load_app_operation_admin_view,
};
use super::install_ops::{validate_grant_targets, validate_grants_within_manifest};
use super::lifecycle_ops::{
    check_management_replay, check_management_replay_after_write, drain_plugin_sync,
    settle_drain_failure,
};
use super::op_settlement::complete_foreground_non_staged_op;
use super::reauth::{ConsentPayload, now_unix};
use super::service::{PluginService, run_app_read, run_blocking};
use super::staging::gc_unreferenced_artifacts_with_store;
use super::types::{OperationAdminView, PluginServiceError, ReauthPurpose};
use super::upgrade_switch_exec::execute_package_switch_sync;
use crate::management::ApiState;

pub(super) struct SwitchRequest {
    pub actor_user_id: domain::UserId,
    pub op_kind: StagedPluginOpKind,
    pub expected_package_hash: Option<String>,
    pub preparation_id: Uuid,
    pub consent_token: String,
    pub grants: Vec<PluginGrant>,
    pub state_policy: String,
    pub target_config_digest: String,
    pub reauth_token: Option<String>,
    pub reauth_purpose: ReauthPurpose,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: store::plugins::CanonicalPluginRequest,
}

struct UpgradeConsentData {
    version: String,
    package_hash: String,
    upload_id: Option<Uuid>,
    target_config: Option<PluginUpgradeConfig>,
    reset_scopes: Option<Vec<store::plugins::PluginSecurityScope>>,
}

impl PluginService {
    pub(super) async fn run_package_switch(
        &self,
        state: &ApiState,
        plugin_id: &str,
        req_in: SwitchRequest,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            req_in.actor_user_id,
            Some(plugin_id),
            &req_in.idempotency_key,
            &req_in.request,
            req_in.op_kind.as_str(),
        )
        .await?
        {
            return Ok(replayed);
        }
        super::service::require_foreground_write_admitted(state)?;

        let validation_host = self
            .validation_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        let host = self
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();

        let (reset_token, consent) =
            match self.peek_switch_tokens(state, plugin_id, &req_in).await? {
                Ok(pair) => pair,
                Err(replayed) => return Ok(replayed),
            };
        let reset_token_owned = reset_token.map(str::to_string);
        let drain_rev = match self.pre_drain_for_switch(state, plugin_id, &req_in).await? {
            Ok(rev) => rev,
            Err(replayed) => return Ok(replayed),
        };
        let pin = self.staging_pins().pin(&consent.package_hash);
        let (mut claim, _active_op_guard) = match begin_and_claim_switch(
            self,
            state,
            plugin_id,
            &req_in,
            &consent,
            drain_rev,
            reset_token_owned,
        )
        .await
        {
            Ok(OpBegin::Replayed(view)) => return Ok(*view),
            Ok(OpBegin::Proceed(pair)) => pair,
            Err(err) => {
                drop(pin);
                return maybe_replay_switch_after_err(state, plugin_id, &req_in, err).await;
            }
        };

        let s = state.clone();
        let pid = plugin_id.to_string();
        let actor = req_in.actor_user_id;
        let grants_exec = req_in.grants;
        let state_policy_exec = if req_in.state_policy == "reset" {
            PluginStatePolicy::Reset {
                scopes: consent.reset_scopes,
            }
        } else {
            PluginStatePolicy::Preserve
        };
        let op_id = claim.operation_id();
        let fail_phase = if req_in.op_kind == StagedPluginOpKind::Rollback {
            "rollback-failed"
        } else {
            "upgrade-failed"
        };

        run_blocking(move || {
            let _guard = _active_op_guard;
            let res = execute_package_switch_sync(
                &s,
                &host,
                &validation_host,
                &mut claim,
                &pid,
                actor,
                consent.upload_id,
                &consent.package_hash,
                &consent.version,
                state_policy_exec,
                consent.target_config,
                grants_exec,
            );
            drop(pin);
            if res
                .as_ref()
                .is_err_and(|e| !super::recovery::is_transient_or_admission_err(e))
            {
                let _ = super::op_settlement::settle_failed_staged_operation_mode(
                    &s, op_id, fail_phase, true,
                );
                let _ = gc_unreferenced_artifacts_with_store(&s.store, s.plugins.staging_pins());
            }
            res
        })
        .await
    }

    async fn peek_switch_tokens<'a>(
        &self,
        state: &ApiState,
        plugin_id: &str,
        req_in: &'a SwitchRequest,
    ) -> Result<Result<(Option<&'a str>, UpgradeConsentData), OperationAdminView>, PluginServiceError>
    {
        check_switch_preconditions(state, req_in).await?;
        let reset_token = match peek_switch_reset_reauth(self, state, plugin_id, req_in).await? {
            Ok(tok) => tok,
            Err(replayed) => return Ok(Err(replayed)),
        };
        let consent = match peek_switch_consent(self, state, plugin_id, req_in).await? {
            Ok(data) => data,
            Err(replayed) => return Ok(Err(replayed)),
        };
        Ok(Ok((reset_token, consent)))
    }

    async fn pre_drain_for_switch(
        &self,
        state: &ApiState,
        plugin_id: &str,
        req_in: &SwitchRequest,
    ) -> Result<Result<i64, OperationAdminView>, PluginServiceError> {
        let current = match load_plugin_for_switch(state, plugin_id, req_in).await? {
            Ok(p) => p,
            Err(replayed) => return Ok(Err(replayed)),
        };
        match ensure_drained_for_switch(self, state, plugin_id, &current, req_in).await {
            Ok(rev) => Ok(Ok(rev)),
            Err(err) => {
                if let Some(replayed) = check_management_replay_after_write(
                    state,
                    req_in.actor_user_id,
                    Some(plugin_id),
                    &req_in.idempotency_key,
                    &req_in.request,
                    req_in.op_kind.as_str(),
                )
                .await?
                {
                    return Ok(Err(replayed));
                }
                Err(err)
            }
        }
    }
}

async fn maybe_replay_switch_after_err(
    state: &ApiState,
    plugin_id: &str,
    req_in: &SwitchRequest,
    err: PluginServiceError,
) -> Result<OperationAdminView, PluginServiceError> {
    if (is_consent_stale_err(&err)
        || is_reauth_invalid_err(&err)
        || matches!(
            err,
            PluginServiceError::Conflict { .. } | PluginServiceError::Store(_)
        ))
        && let Some(replayed) = check_management_replay_after_write(
            state,
            req_in.actor_user_id,
            Some(plugin_id),
            &req_in.idempotency_key,
            &req_in.request,
            req_in.op_kind.as_str(),
        )
        .await?
    {
        return Ok(replayed);
    }
    Err(err)
}

async fn check_switch_preconditions(
    state: &ApiState,
    req_in: &SwitchRequest,
) -> Result<(), PluginServiceError> {
    if !matches!(req_in.state_policy.as_str(), "preserve" | "reset") {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "state_policy must be `preserve` or `reset`",
        ));
    }
    if req_in.target_config_digest.trim().is_empty() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "target_config_digest is required",
        ));
    }
    if req_in
        .expected_package_hash
        .as_deref()
        .is_some_and(|h| h.trim().is_empty())
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "package_hash is required",
        ));
    }
    let g_check = req_in.grants.clone();
    run_app_read(state, move |store| validate_grant_targets(store, &g_check)).await?;
    Ok(())
}

async fn peek_switch_reset_reauth<'a>(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    req_in: &'a SwitchRequest,
) -> Result<Result<Option<&'a str>, OperationAdminView>, PluginServiceError> {
    if req_in.state_policy != "reset" {
        return Ok(Ok(None));
    }
    let token = req_in.reauth_token.as_deref().ok_or_else(|| {
        PluginServiceError::forbidden(
            "plugin.reauth_required",
            "reauth_token is required when state_policy is reset",
        )
    })?;
    if let Err(err) = service
        .peek_reauth_token(state, req_in.actor_user_id, req_in.reauth_purpose, token)
        .await
    {
        if is_reauth_invalid_err(&err)
            && let Some(replayed) = check_management_replay_after_write(
                state,
                req_in.actor_user_id,
                Some(plugin_id),
                &req_in.idempotency_key,
                &req_in.request,
                req_in.op_kind.as_str(),
            )
            .await?
        {
            return Ok(Err(replayed));
        }
        return Err(err);
    }
    Ok(Ok(Some(token)))
}

fn validate_switch_consent_payload(
    p: &ConsentPayload,
    plugin_id: &str,
    req_in: &SwitchRequest,
) -> Result<(), PluginServiceError> {
    match p {
        ConsentPayload::Upgrade {
            plugin_id: pid,
            package_hash,
            upload_id,
            base_revision,
            pre_drained_to_revision,
            state_policy,
            target_config_digest,
            allowed_capabilities,
            ..
        } if pid == plugin_id
            && (*base_revision == req_in.expected_revision
                || *pre_drained_to_revision == Some(req_in.expected_revision))
            && state_policy == &req_in.state_policy
            && target_config_digest == &req_in.target_config_digest
            && req_in
                .expected_package_hash
                .as_deref()
                .is_none_or(|h| h == package_hash)
            && (req_in.op_kind != StagedPluginOpKind::Rollback || upload_id.is_none()) =>
        {
            validate_grants_within_manifest(&req_in.grants, allowed_capabilities)
        }
        _ => Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "consent_token does not match upgrade/rollback parameters",
        )),
    }
}

async fn peek_switch_consent(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    req_in: &SwitchRequest,
) -> Result<Result<UpgradeConsentData, OperationAdminView>, PluginServiceError> {
    match service.consent().peek(
        req_in.actor_user_id,
        Some(req_in.preparation_id),
        &req_in.consent_token,
        |p| {
            validate_switch_consent_payload(p, plugin_id, req_in)?;
            let ConsentPayload::Upgrade {
                version,
                package_hash,
                upload_id,
                target_config,
                reset_scopes,
                ..
            } = p
            else {
                return Err(PluginServiceError::conflict(
                    "plugin.consent_stale",
                    "invalid upgrade consent payload",
                ));
            };
            Ok(UpgradeConsentData {
                version: version.clone(),
                package_hash: package_hash.clone(),
                upload_id: *upload_id,
                target_config: target_config.clone(),
                reset_scopes: reset_scopes.clone(),
            })
        },
    ) {
        Ok(data) => Ok(Ok(data)),
        Err(err) => {
            if is_consent_stale_err(&err)
                && let Some(replayed) = check_management_replay_after_write(
                    state,
                    req_in.actor_user_id,
                    Some(plugin_id),
                    &req_in.idempotency_key,
                    &req_in.request,
                    req_in.op_kind.as_str(),
                )
                .await?
            {
                return Ok(Err(replayed));
            }
            Err(err)
        }
    }
}

async fn load_plugin_for_switch(
    state: &ApiState,
    plugin_id: &str,
    req_in: &SwitchRequest,
) -> Result<Result<PluginRecord, OperationAdminView>, PluginServiceError> {
    let pid_read = plugin_id.to_string();
    let current = match run_app_read(state, move |store| {
        store
            .get_plugin(&pid_read)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))
    })
    .await
    {
        Ok(p) => p,
        Err(err) => {
            if let Some(replayed) = check_management_replay_after_write(
                state,
                req_in.actor_user_id,
                Some(plugin_id),
                &req_in.idempotency_key,
                &req_in.request,
                req_in.op_kind.as_str(),
            )
            .await?
            {
                return Ok(Err(replayed));
            }
            return Err(err);
        }
    };
    if current.revision != req_in.expected_revision {
        if let Some(replayed) = check_management_replay_after_write(
            state,
            req_in.actor_user_id,
            Some(plugin_id),
            &req_in.idempotency_key,
            &req_in.request,
            req_in.op_kind.as_str(),
        )
        .await?
        {
            return Ok(Err(replayed));
        }
        return Err(PluginServiceError::conflict(
            "plugin.revision_conflict",
            "plugin revision mismatch",
        ));
    }
    Ok(Ok(current))
}

async fn ensure_drained_for_switch(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    current: &PluginRecord,
    req_in: &SwitchRequest,
) -> Result<i64, PluginServiceError> {
    if current.lifecycle == PluginLifecycle::Enabled {
        let pid = plugin_id.to_string();
        let rev = current.revision;
        let actor = req_in.actor_user_id;
        let st_guard = state.clone();
        let (draining, claim, _op_guard) = run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: format!("switch-drain:{pid}:{rev}:{}", Uuid::new_v4()),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid}/disable"),
                    body_digest: [0; 32],
                },
                kind: "disable".into(),
                expected_revision: Some(rev),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("switch-drain:{pid}")),
            })?;
            let op_id = accepted.operation.operation_id;
            let draining = app.begin_plugin_lifecycle(&pid, rev, op_id, false)?;
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            let guard = st_guard.plugins.try_acquire_staged_op(op_id);
            Ok((draining, claim, guard))
        })
        .await?;
        if draining.lifecycle == PluginLifecycle::Draining {
            service
                .in_flight()
                .close_dispatch(plugin_id, draining.generation);
        }
        service.consent().mark_upgrade_pre_drained(
            req_in.actor_user_id,
            req_in.preparation_id,
            &req_in.consent_token,
            rev,
            draining.revision.saturating_add(1),
        );
        let s = state.clone();
        let pid = plugin_id.to_string();
        let op_id = claim.operation_id();
        let drained = run_blocking(move || {
            let _guard = _op_guard;
            let res = match drain_plugin_sync(&s.plugins, &s, &pid, op_id, &draining, None) {
                Ok(r) => r,
                Err(err) => {
                    settle_drain_failure(&s, &claim, &err);
                    return Err(err);
                }
            };
            complete_foreground_non_staged_op(&s, &claim, true, "disabled")?;
            super::registry_sync::sync_registry_snapshot(&s, true)?;
            Ok(res)
        })
        .await?;
        Ok(drained.revision)
    } else if matches!(
        current.lifecycle,
        PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
    ) && !current.desired_enabled
    {
        if let Some(active) = current.active_operation_id {
            return Err(PluginServiceError::Store(
                PluginError::OperationActive(active).into(),
            ));
        }
        Ok(current.revision)
    } else {
        Err(PluginServiceError::conflict(
            "plugin.not_drained",
            "plugin is currently enabling or draining",
        ))
    }
}

async fn begin_and_claim_switch(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    req_in: &SwitchRequest,
    consent: &UpgradeConsentData,
    drain_rev: i64,
    reset_token: Option<String>,
) -> Result<
    OpBegin<(
        store::plugins::PluginCommandClaim,
        Option<super::service::ActiveStagedOpGuard>,
    )>,
    PluginServiceError,
> {
    let pid = plugin_id.to_string();
    let pkg_hash = consent.package_hash.clone();
    let ver = consent.version.clone();
    let op_kind = req_in.op_kind;
    let actor = req_in.actor_user_id;
    let prep_id = req_in.preparation_id;
    let consent_tok = req_in.consent_token.clone();
    let consent_store = service.consent().clone();
    let reauth = service.reauth().clone();
    let reauth_purpose = req_in.reauth_purpose;
    let exp_rev = req_in.expected_revision;
    let exp_pkg_hash = req_in.expected_package_hash.clone();
    let state_pol = req_in.state_policy.clone();
    let digest = req_in.target_config_digest.clone();
    let grants_for_op = req_in.grants.clone();
    let grants_for_check = req_in.grants.clone();
    let idem = req_in.idempotency_key.clone();
    let req = req_in.request.clone();
    let upload_id = consent.upload_id;
    let st_guard = state.clone();
    run_app_write_tx(state, move |app| {
        let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: idem.clone(),
            request: req.clone(),
            op_kind,
            expected_revision: drain_rev,
            intent: StagedPluginIntent {
                target_plugin_id: pid.clone(),
                package_hash: Some(pkg_hash),
                version: Some(ver),
                artifact_path: upload_id.map(|id| format!("plugins/uploads/{id}/package.cmplugin")),
                state_policy: Some(state_pol.clone()),
                target_config_digest: Some(digest.clone()),
                grants: grants_for_op,
            },
            cause: PluginCommandCause::root(format!("{}:{pid}", op_kind.as_str())),
        })?;
        if accepted.replayed {
            return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                app,
                accepted.operation.operation_id,
            )?)));
        }
        validate_grant_targets(app, &grants_for_check)?;
        let claim = app.claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            now_unix(),
        )?;
        let check_req = SwitchRequest {
            actor_user_id: actor,
            op_kind,
            expected_package_hash: exp_pkg_hash,
            preparation_id: prep_id,
            consent_token: consent_tok.clone(),
            grants: grants_for_check,
            state_policy: state_pol,
            target_config_digest: digest,
            reauth_token: reset_token.clone(),
            reauth_purpose,
            expected_revision: exp_rev,
            idempotency_key: idem,
            request: req,
        };
        consent_store.peek(actor, Some(prep_id), &consent_tok, |p| {
            validate_switch_consent_payload(p, &pid, &check_req)
        })?;
        if let Some(tok) = reset_token.as_deref() {
            reauth.consume_in_app_tx(app, actor, reauth_purpose, tok)?;
        }
        let _ = consent_store.peek_consume(actor, Some(prep_id), &consent_tok, |p| {
            validate_switch_consent_payload(p, &pid, &check_req)
        })?;
        let guard = st_guard.plugins.try_acquire_staged_op(claim.operation_id());
        Ok(OpBegin::Proceed((claim, guard)))
    })
    .await
}
