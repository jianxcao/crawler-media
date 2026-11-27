use domain::UserId;
use plugin_package::PluginManifest;
use store::StoreError;
use store::plugins::{
    AcceptPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginError,
    PluginLifecycle, PluginLifecycleObservation, PluginRecord, PluginSubject,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    OpBegin, is_consent_stale_err, is_reauth_invalid_err, load_app_operation_admin_view,
};
use super::install_ops::{
    manifest_required_capabilities, validate_grant_targets, validate_grants_within_manifest,
};
use super::lifecycle_ops::{
    check_management_replay, check_management_replay_after_write, drain_plugin_sync,
    load_operation_admin_view, settle_drain_failure, sync_all_plugin_tasks,
};
use super::op_settlement::{complete_foreground_non_staged_op, complete_non_staged_app_op};
use super::reauth::{ConsentPayload, digest_grants, now_unix};
use super::recovery::settle_interrupted_lifecycle_operation;
use super::service::{
    PluginService, run_app_read, run_blocking, run_foreground_app_tx, run_foreground_store_sync,
};
use super::types::{
    DisableDeveloperModeInput, GrantPermissionsInput, OperationAdminView, PluginServiceError,
    ReauthPurpose, RevokePermissionsInput, RevokeTrustKeyInput,
};
use crate::management::ApiState;

impl PluginService {
    pub async fn grant_permissions(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: GrantPermissionsInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "grant-permissions",
        )
        .await?
        {
            return Ok(replayed);
        }

        let expected_digest = digest_grants(&input.grants)?;
        let expected_rev = input.expected_revision;
        let grants_clone = input.grants.clone();
        let plugin = load_plugin_for_grant(state, plugin_id, &grants_clone).await?;
        let current_hash = plugin.package_hash.clone();

        if let Err(err) = validate_and_check_grant(
            self,
            plugin_id,
            &plugin,
            &input,
            expected_digest,
            &current_hash,
        ) {
            if (is_consent_stale_err(&err)
                || matches!(
                    err,
                    PluginServiceError::Conflict { .. }
                        | PluginServiceError::Store(StoreError::Plugin(
                            PluginError::Conflict | PluginError::NotDrained
                        ))
                ))
                && let Some(replayed) = check_management_replay_after_write(
                    state,
                    input.actor_user_id,
                    Some(plugin_id),
                    &input.idempotency_key,
                    &input.request,
                    "grant-permissions",
                )
                .await?
            {
                return Ok(replayed);
            }
            return Err(err);
        }

        let pid = plugin_id.to_string();
        let actor = input.actor_user_id;
        let idem_retry = input.idempotency_key.clone();
        let req_retry = input.request.clone();
        let write_res = commit_grant_permissions_tx(
            state,
            self.consent().clone(),
            pid,
            actor,
            expected_rev,
            expected_digest,
            grants_clone,
            input,
        )
        .await;
        match write_res {
            Ok(view) => Ok(view),
            Err(err) => {
                if (is_consent_stale_err(&err)
                    || matches!(
                        err,
                        PluginServiceError::Conflict { .. }
                            | PluginServiceError::Store(StoreError::Plugin(
                                PluginError::Conflict | PluginError::NotDrained
                            ))
                    ))
                    && let Some(replayed) = check_management_replay_after_write(
                        state,
                        actor,
                        Some(plugin_id),
                        &idem_retry,
                        &req_retry,
                        "grant-permissions",
                    )
                    .await?
                {
                    return Ok(replayed);
                }
                Err(err)
            }
        }
    }

    pub async fn revoke_permissions(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: RevokePermissionsInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "revoke-permissions",
        )
        .await?
        {
            return Ok(replayed);
        }

        let pid_read = plugin_id.to_string();
        let current = run_app_read(state, move |store| {
            store
                .get_plugin(&pid_read)?
                .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))
        })
        .await?;
        if current.revision != input.expected_revision {
            if let Some(replayed) = check_management_replay_after_write(
                state,
                input.actor_user_id,
                Some(plugin_id),
                &input.idempotency_key,
                &input.request,
                "revoke-permissions",
            )
            .await?
            {
                return Ok(replayed);
            }
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "plugin revision mismatch",
            ));
        }

        let pid = plugin_id.to_string();
        let actor = input.actor_user_id;
        let expected_rev = input.expected_revision;
        let grants = input.grants.clone();
        let idem = input.idempotency_key;
        let req = input.request;
        let st_guard = state.clone();
        let (updated, claim, _op_guard) = match run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: idem,
                request: req,
                kind: "revoke-permissions".into(),
                expected_revision: Some(expected_rev),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("revoke:{pid}")),
            })?;
            let op_id = accepted.operation.operation_id;
            if accepted.replayed {
                return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                    app, op_id,
                )?)));
            }
            let updated = app.revoke_plugin_permissions(
                &pid,
                &current.package_hash,
                expected_rev,
                &grants,
                Some(op_id),
            )?;
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            let guard = st_guard.plugins.try_acquire_staged_op(op_id);
            Ok(OpBegin::Proceed((updated, claim, guard)))
        })
        .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(triple) => triple,
        };

        let op_id = claim.operation_id();
        if updated.lifecycle == PluginLifecycle::Draining {
            self.in_flight()
                .close_dispatch(plugin_id, updated.generation);
            let s = state.clone();
            let pid = plugin_id.to_string();
            let reason = input.reason;
            return run_blocking(move || {
                let _guard = _op_guard;
                if let Err(err) =
                    drain_plugin_sync(&s.plugins, &s, &pid, op_id, &updated, Some(&reason))
                {
                    settle_drain_failure(&s, &claim, &err);
                    return Err(err);
                }
                complete_foreground_non_staged_op(&s, &claim, true, "revoked")?;
                super::registry_sync::sync_registry_snapshot(&s, true)?;
                load_operation_admin_view(&s.store, op_id)
            })
            .await;
        }
        self.in_flight()
            .close_dispatch(plugin_id, updated.generation.saturating_add(1));
        run_app_write_tx(state, move |app| {
            let _guard = _op_guard;
            complete_non_staged_app_op(app, &claim, true, "revoked")?;
            load_app_operation_admin_view(app, op_id)
        })
        .await
    }

    pub async fn revoke_trust_key(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        input: RevokeTrustKeyInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            actor_user_id,
            None,
            &input.idempotency_key,
            &input.request,
            "revoke-trust-key",
        )
        .await?
        {
            return Ok(replayed);
        }
        if input.reason.len() > 1024 {
            return Err(PluginServiceError::Store(
                PluginError::Invalid("revoked reason exceeds limit").into(),
            ));
        }
        let idem_retry = input.idempotency_key.clone();
        let req_retry = input.request.clone();
        let (active_plugins, claim) =
            match commit_revoke_trust_key_tx(state, self.reauth().clone(), actor_user_id, input)
                .await
            {
                Ok(OpBegin::Replayed(view)) => return Ok(*view),
                Ok(OpBegin::Proceed(pair)) => pair,
                Err(err) => {
                    if (is_reauth_invalid_err(&err)
                        || matches!(
                            err,
                            PluginServiceError::Conflict { .. }
                                | PluginServiceError::Store(StoreError::Plugin(
                                    PluginError::Conflict
                                ))
                        ))
                        && let Some(replayed) = check_management_replay_after_write(
                            state,
                            actor_user_id,
                            None,
                            &idem_retry,
                            &req_retry,
                            "revoke-trust-key",
                        )
                        .await?
                    {
                        return Ok(replayed);
                    }
                    return Err(err);
                }
            };

        cascade_drain_plugins(
            self,
            state,
            actor_user_id,
            active_plugins,
            claim,
            "signing-key-revoked",
            "revoked",
        )
        .await
    }

    pub async fn disable_developer_mode(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        input: DisableDeveloperModeInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            actor_user_id,
            None,
            &input.idempotency_key,
            &input.request,
            "disable-developer-mode",
        )
        .await?
        {
            return Ok(replayed);
        }

        let expected_rev = input.expected_revision;
        let idem = input.idempotency_key.clone();
        let req = input.request.clone();
        let (active_unsigned, claim) = match run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: None,
                subject: PluginSubject::User(actor_user_id),
                idempotency_key: idem,
                request: req,
                kind: "disable-developer-mode".into(),
                expected_revision: None,
                expected_generation: None,
                cause: PluginCommandCause::root("disable-dev-mode"),
            })?;
            let op_id = accepted.operation.operation_id;
            if accepted.replayed {
                return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                    app, op_id,
                )?)));
            }
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            app.set_plugin_developer_mode(false, expected_rev, &actor_user_id.to_string())?;
            let active = app.list_active_unsigned_plugins()?;
            Ok(OpBegin::Proceed((active, claim)))
        })
        .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(pair) => pair,
        };

        cascade_drain_plugins(
            self,
            state,
            actor_user_id,
            active_unsigned,
            claim,
            "developer-mode-disabled",
            "disabled",
        )
        .await
    }
}

async fn cascade_drain_plugins(
    service: &PluginService,
    state: &ApiState,
    actor_user_id: UserId,
    plugins: Vec<PluginRecord>,
    claim: store::plugins::PluginCommandClaim,
    reason: &'static str,
    done_phase: &'static str,
) -> Result<OperationAdminView, PluginServiceError> {
    for p in &plugins {
        service
            .in_flight()
            .close_dispatch(&p.plugin_id, p.generation + 1);
    }
    let s = state.clone();
    let op_id = claim.operation_id();
    run_blocking(move || {
        let mut first_err: Option<PluginServiceError> = None;
        for p in plugins {
            if let Err(err) = drain_existing_active_plugin(&s, actor_user_id, p, reason) {
                first_err.get_or_insert(err);
            }
        }
        if let Some(err) = first_err {
            if !super::recovery::is_transient_or_admission_err(&err) {
                let _ = complete_foreground_non_staged_op(&s, &claim, false, "drain-incomplete");
            }
            return Err(err);
        }
        complete_foreground_non_staged_op(&s, &claim, true, done_phase)?;
        load_operation_admin_view(&s.store, op_id)
    })
    .await
}

fn drain_existing_active_plugin(
    state: &ApiState,
    actor: UserId,
    plugin: PluginRecord,
    reason: &str,
) -> Result<(), PluginServiceError> {
    let pid = plugin.plugin_id.clone();
    state
        .plugins
        .in_flight()
        .close_dispatch(&pid, plugin.generation + 1);
    if plugin.lifecycle == PluginLifecycle::Enabling
        && let Some(active_op) = plugin.active_operation_id
    {
        let finalized = run_foreground_store_sync(state, |store| {
            match store.finalize_plugin_lifecycle_checked(
                &PluginLifecycleObservation {
                    plugin_id: pid.clone(),
                    operation_id: active_op,
                    revision: plugin.revision,
                    generation: plugin.generation,
                    workers_exited: true,
                    unknown_effects_reconciled: true,
                    validation_succeeded: false,
                },
                Some(reason),
            ) {
                Ok(_) => {
                    let _ = settle_interrupted_lifecycle_operation(store, active_op, false, reason);
                    Ok(true)
                }
                Err(StoreError::Plugin(PluginError::Conflict)) => {
                    if store.get_plugin(&pid)?.is_none_or(|cur| {
                        matches!(
                            cur.lifecycle,
                            PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
                        )
                    }) {
                        Ok(false)
                    } else {
                        Err(PluginServiceError::Store(PluginError::Conflict.into()))
                    }
                }
                Err(err) => Err(err.into()),
            }
        })?;
        if finalized {
            let _ = sync_all_plugin_tasks(state, &pid);
        }
        return Ok(());
    }
    if plugin.lifecycle != PluginLifecycle::Enabled {
        return Ok(());
    }
    let (draining, claim, _op_guard) = match run_foreground_app_tx(state, true, |app| {
        let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.clone()),
            subject: PluginSubject::User(actor),
            idempotency_key: format!("cascade-drain:{pid}:{}", Uuid::new_v4()),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{pid}/disable"),
                body_digest: [0; 32],
            },
            kind: "disable".into(),
            expected_revision: Some(plugin.revision),
            expected_generation: None,
            cause: PluginCommandCause::root(format!("cascade:{reason}:{pid}")),
        })?;
        let op_id = accepted.operation.operation_id;
        let draining = app.begin_plugin_lifecycle(&pid, plugin.revision, op_id, false)?;
        let claim =
            app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
        let guard = state.plugins.try_acquire_staged_op(op_id);
        Ok((draining, claim, guard))
    }) {
        Ok(triple) => triple,
        Err(PluginServiceError::Store(StoreError::Plugin(PluginError::Conflict))) => {
            if state.store.get_plugin(&pid)?.is_none_or(|cur| {
                matches!(
                    cur.lifecycle,
                    PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
                )
            }) {
                return Ok(());
            }
            return Err(PluginServiceError::Store(PluginError::Conflict.into()));
        }
        Err(err) => return Err(err),
    };
    let op_id = claim.operation_id();
    if let Err(err) = drain_plugin_sync(&state.plugins, state, &pid, op_id, &draining, Some(reason))
    {
        settle_drain_failure(state, &claim, &err);
        return Err(err);
    }
    complete_foreground_non_staged_op(state, &claim, true, reason)?;
    super::registry_sync::sync_registry_snapshot(state, true)?;
    Ok(())
}

async fn load_plugin_for_grant(
    state: &ApiState,
    plugin_id: &str,
    grants: &[store::plugins::PluginGrant],
) -> Result<PluginRecord, PluginServiceError> {
    let pid = plugin_id.to_string();
    let g_check = grants.to_vec();
    run_app_read(state, move |store| {
        validate_grant_targets(store, &g_check)?;
        store
            .get_plugin(&pid)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))
    })
    .await
}

fn match_grant_consent(
    payload: &ConsentPayload,
    plugin_id: &str,
    current_hash: &str,
    expected_rev: i64,
    expected_digest: [u8; 32],
) -> Result<(), PluginServiceError> {
    match payload {
        ConsentPayload::Permissions {
            plugin_id: p,
            package_hash,
            base_revision,
            grants_digest,
        } if p == plugin_id
            && package_hash == current_hash
            && *base_revision == expected_rev
            && *grants_digest == expected_digest =>
        {
            Ok(())
        }
        _ => Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "permissions consent_token does not match plugin, revision, or grants",
        )),
    }
}

fn validate_and_check_grant(
    service: &PluginService,
    plugin_id: &str,
    plugin: &PluginRecord,
    input: &GrantPermissionsInput,
    expected_digest: [u8; 32],
    current_hash: &str,
) -> Result<(), PluginServiceError> {
    service
        .consent()
        .peek(input.actor_user_id, None, &input.consent_token, |payload| {
            match_grant_consent(
                payload,
                plugin_id,
                current_hash,
                input.expected_revision,
                expected_digest,
            )
        })?;
    if plugin.revision != input.expected_revision || plugin.revision == i64::MAX {
        return Err(PluginServiceError::Store(PluginError::Conflict.into()));
    }
    if !matches!(
        plugin.lifecycle,
        PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
    ) || plugin.desired_enabled
        || plugin.active_operation_id.is_some()
    {
        return Err(PluginServiceError::Store(PluginError::NotDrained.into()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn commit_grant_permissions_tx(
    state: &ApiState,
    consent: std::sync::Arc<super::reauth::ConsentStore>,
    pid: String,
    actor: UserId,
    expected_rev: i64,
    expected_digest: [u8; 32],
    grants: Vec<store::plugins::PluginGrant>,
    input: GrantPermissionsInput,
) -> Result<OperationAdminView, PluginServiceError> {
    let idem = input.idempotency_key;
    let req = input.request;
    let consent_tok = input.consent_token;
    let st_fence = state.clone();
    run_app_write_tx(state, move |app| {
        if let Some(op) = app.get_plugin_operation_by_idempotency(
            &PluginSubject::User(actor),
            Some(&pid),
            &idem,
            &req,
            "grant-permissions",
        )? {
            return load_app_operation_admin_view(app, op.operation_id);
        }
        let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.clone()),
            subject: PluginSubject::User(actor),
            idempotency_key: idem,
            request: req,
            kind: "grant-permissions".into(),
            expected_revision: Some(expected_rev),
            expected_generation: None,
            cause: PluginCommandCause::root(format!("grant:{pid}")),
        })?;
        let op_id = accepted.operation.operation_id;
        if accepted.replayed {
            return load_app_operation_admin_view(app, op_id);
        }
        let p = app
            .get_plugin(&pid)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        let ver = app
            .get_plugin_version(&pid, &p.package_hash)?
            .ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin version missing")
            })?;
        if ver.manifest_json != "{}" {
            let manifest = serde_json::from_str::<PluginManifest>(&ver.manifest_json)
                .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            validate_grants_within_manifest(&grants, &manifest_required_capabilities(&manifest))?;
        }
        validate_grant_targets(app, &grants)?;
        let claim =
            app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
        app.grant_plugin_permissions(&pid, &p.package_hash, expected_rev, &grants)?;
        let _ = consent.peek_consume(actor, None, &consent_tok, |payload| {
            match_grant_consent(
                payload,
                &pid,
                &p.package_hash,
                expected_rev,
                expected_digest,
            )
        })?;
        complete_non_staged_app_op(app, &claim, true, "granted")?;
        if let Some(updated) = app.get_plugin(&pid)? {
            st_fence
                .plugins
                .in_flight()
                .close_dispatch(&pid, updated.generation.saturating_add(1));
        }
        load_app_operation_admin_view(app, op_id)
    })
    .await
}

async fn commit_revoke_trust_key_tx(
    state: &ApiState,
    reauth: std::sync::Arc<super::reauth::ReauthService>,
    actor_user_id: UserId,
    input: RevokeTrustKeyInput,
) -> Result<OpBegin<(Vec<PluginRecord>, store::plugins::PluginCommandClaim)>, PluginServiceError> {
    let key_id = input.key_id;
    let reason = input.reason;
    let expected_rev = input.expected_revision;
    let idem = input.idempotency_key;
    let req = input.request;
    let reauth_tok = input.reauth_token;
    run_app_write_tx(state, move |app| {
        if let Some(op) = app.get_plugin_operation_by_idempotency(
            &PluginSubject::User(actor_user_id),
            None,
            &idem,
            &req,
            "revoke-trust-key",
        )? {
            return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                app,
                op.operation_id,
            )?)));
        }
        reauth.peek_in_app_tx(
            app,
            actor_user_id,
            ReauthPurpose::TrustKeyRevoke,
            &reauth_tok,
        )?;
        let key = app.get_plugin_trust_key(&key_id)?.ok_or_else(|| {
            PluginServiceError::not_found("plugin.missing", "trust key not found")
        })?;
        if key.revision != expected_rev {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "trust key revision mismatch",
            ));
        }
        let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: None,
            subject: PluginSubject::User(actor_user_id),
            idempotency_key: idem,
            request: req,
            kind: "revoke-trust-key".into(),
            expected_revision: None,
            expected_generation: None,
            cause: PluginCommandCause::root(format!("revoke-key:{key_id}")),
        })?;
        let op_id = accepted.operation.operation_id;
        let claim =
            app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
        app.revoke_plugin_trust_key(
            &key_id,
            Some(expected_rev),
            &reason,
            &actor_user_id.to_string(),
        )?;
        let active = app.list_active_plugins_for_trust_key(&key_id)?;
        reauth.consume_in_app_tx(
            app,
            actor_user_id,
            ReauthPurpose::TrustKeyRevoke,
            &reauth_tok,
        )?;
        Ok(OpBegin::Proceed((active, claim)))
    })
    .await
}
