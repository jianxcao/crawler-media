use std::time::{Duration, Instant};

use domain::{PluginTaskBindingId, UserId};
use store::StoreError;
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginCommandClaim, PluginError, PluginLifecycle, PluginLifecycleObservation,
    PluginOperationState, PluginRecord, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::{run_app_write_tx, validate_config_for_enable};
use super::idempotency::{OpBegin, load_app_operation_admin_view};
use super::op_settlement::complete_non_staged_app_op;
pub(crate) use super::op_settlement::{
    check_management_replay, check_management_replay_after_write, complete_non_staged_op,
    load_operation_admin_view, op_to_admin_view, settle_drain_failure,
};
use super::reauth::now_unix;
use super::service::{PluginService, run_app_read, run_blocking, run_foreground_app_tx};
use super::staging::gc_unreferenced_artifacts_with_store;
use super::tasks::sync_task_binding_with_handles;
use super::types::{
    DisablePluginInput, EnablePluginInput, OperationAdminView, PluginServiceError,
    UninstallPluginInput,
};
use super::wasm_runner::validate_installed_components;
use crate::management::ApiState;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn sync_all_plugin_tasks(
    state: &ApiState,
    plugin_id: &str,
) -> Result<(), PluginServiceError> {
    let mut after: Option<String> = None;
    loop {
        let page = state
            .store
            .list_plugin_task_bindings(plugin_id, after.as_deref(), Some(200))?;
        for binding in &page.items {
            sync_task_binding_with_handles(&state.store, &state.jobs, binding)?;
        }
        match page.next_cursor {
            Some(next) if Some(&next) != after.as_ref() => after = Some(next),
            _ => break,
        }
    }
    Ok(())
}

pub(crate) fn has_active_plugin_job(state: &ApiState, plugin_id: &str) -> bool {
    let binding_ids: Vec<Uuid> = {
        let attempts = state.active_job_attempts.lock();
        attempts
            .values()
            .filter_map(|job| match job.scope {
                jobs::JobScope::PluginTask(bid) => Some(bid.as_uuid()),
                _ => None,
            })
            .collect()
    };
    binding_ids.into_iter().any(|bid| {
        state
            .store
            .get_plugin_task_binding_by_id(bid)
            .ok()
            .flatten()
            .is_some_and(|b| b.plugin_id == plugin_id)
    })
}

pub(crate) fn drain_plugin_sync(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    op_id: Uuid,
    draining_plugin: &PluginRecord,
    failure_reason: Option<&str>,
) -> Result<PluginRecord, PluginServiceError> {
    drain_plugin_sync_mode(
        service,
        state,
        plugin_id,
        op_id,
        draining_plugin,
        failure_reason,
        true,
    )
}

pub(crate) fn drain_plugin_sync_mode(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    op_id: Uuid,
    draining_plugin: &PluginRecord,
    failure_reason: Option<&str>,
    foreground: bool,
) -> Result<PluginRecord, PluginServiceError> {
    service
        .in_flight()
        .close_dispatch(plugin_id, draining_plugin.generation + 1);
    let _ = super::registry_sync::sync_registry_snapshot(state, true);
    sync_all_plugin_tasks(state, plugin_id)?;

    let deadline = Instant::now() + DRAIN_TIMEOUT;
    let in_flight_ok = service.in_flight().wait_drained(
        plugin_id,
        deadline.saturating_duration_since(Instant::now()),
    );
    let runtime_ok = service.maybe_runtime().is_none_or(|rt| {
        rt.wait_plugin_drained(
            plugin_id,
            deadline.saturating_duration_since(Instant::now()),
        )
    });

    while Instant::now() < deadline && has_active_plugin_job(state, plugin_id) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let jobs_ok = !has_active_plugin_job(state, plugin_id);

    let _ = super::reconcile::reconcile_drain_effects(state, now_unix());
    let unresolved = state
        .store
        .unresolved_plugin_commands(None, Some(200))?
        .into_iter()
        .any(|p| {
            p.operation.plugin_id.as_deref() == Some(plugin_id) && p.operation.operation_id != op_id
        });

    let workers_exited = in_flight_ok && runtime_ok && jobs_ok;
    if !workers_exited || unresolved {
        return Err(PluginServiceError::conflict(
            "plugin.not_drained",
            "plugin still has active workers or unresolved effects",
        ));
    }
    let finalized = run_foreground_app_tx(state, foreground, |app| {
        Ok(app.finalize_plugin_lifecycle_checked(
            &PluginLifecycleObservation {
                plugin_id: plugin_id.to_string(),
                operation_id: op_id,
                revision: draining_plugin.revision,
                generation: draining_plugin.generation,
                workers_exited: true,
                unknown_effects_reconciled: true,
                validation_succeeded: true,
            },
            failure_reason,
        )?)
    })?;
    sync_all_plugin_tasks(state, plugin_id)?;
    Ok(finalized)
}

fn finalize_enable_in_store(
    app: &store::App<'_>,
    plugin_id: &str,
    snapshot: &store::plugins::PluginActivationSnapshot,
    claim: &PluginCommandClaim,
    succeeded: bool,
    phase: &str,
    fail_reason: Option<&str>,
) -> Result<bool, PluginServiceError> {
    let (mut succeeded, mut phase, mut fail_reason) = (succeeded, phase, fail_reason);
    if succeeded {
        let latest_cfg = app.get_plugin_config(plugin_id)?;
        if let Err((reason, _)) =
            validate_config_for_enable(app, plugin_id, &snapshot.version, latest_cfg.as_ref())
        {
            succeeded = false;
            phase = reason;
            fail_reason = Some(reason);
        }
    }
    let op_id = claim.operation_id();
    match app.finalize_plugin_lifecycle_checked(
        &PluginLifecycleObservation {
            plugin_id: plugin_id.to_string(),
            operation_id: op_id,
            revision: snapshot.plugin.revision,
            generation: snapshot.plugin.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: succeeded,
        },
        fail_reason,
    ) {
        Ok(finalized) => {
            let policy_rejected = succeeded && finalized.lifecycle != PluginLifecycle::Enabled;
            let final_ok = succeeded && !policy_rejected;
            let final_phase = if policy_rejected {
                finalized
                    .failure_reason
                    .as_deref()
                    .unwrap_or("activation-policy-denied")
            } else {
                phase
            };
            complete_non_staged_app_op(app, claim, final_ok, final_phase)?;
            Ok(final_ok)
        }
        Err(StoreError::Plugin(PluginError::Conflict)) => {
            if let Some(admin) = app.get_plugin_operation_admin(op_id)?
                && !admin.operation.state.is_terminal()
            {
                let _ = complete_non_staged_app_op(app, claim, false, "aborted");
            }
            Ok(false)
        }
        Err(err) => Err(err.into()),
    }
}

async fn load_plugin_for_uninstall(
    state: &ApiState,
    plugin_id: &str,
    input: &UninstallPluginInput,
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
                input.actor_user_id,
                Some(plugin_id),
                &input.idempotency_key,
                &input.request,
                "uninstall",
            )
            .await?
            {
                return Ok(Err(replayed));
            }
            return Err(err);
        }
    };
    if current.revision != input.expected_revision {
        if let Some(replayed) = check_management_replay_after_write(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "uninstall",
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

async fn begin_staged_uninstall_claim(
    state: &ApiState,
    plugin_id: &str,
    input: &UninstallPluginInput,
    drain_rev: i64,
) -> Result<
    OpBegin<(
        PluginCommandClaim,
        Option<super::service::ActiveStagedOpGuard>,
    )>,
    PluginServiceError,
> {
    let pid = plugin_id.to_string();
    let actor = input.actor_user_id;
    let idem = input.idempotency_key.clone();
    let req = input.request.clone();
    let state_policy = if input.remove_private_data {
        "remove"
    } else {
        "preserve"
    }
    .to_string();
    let st_guard = state.clone();
    run_app_write_tx(state, move |app| {
        let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: idem,
            request: req,
            op_kind: StagedPluginOpKind::Uninstall,
            expected_revision: drain_rev,
            intent: StagedPluginIntent {
                target_plugin_id: pid.clone(),
                package_hash: None,
                version: None,
                artifact_path: None,
                state_policy: Some(state_policy),
                target_config_digest: None,
                grants: Vec::new(),
            },
            cause: PluginCommandCause::root(format!("uninstall:{pid}")),
        })?;
        if accepted.replayed {
            return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                app,
                accepted.operation.operation_id,
            )?)));
        }
        let claim = app.claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            now_unix(),
        )?;
        let guard = st_guard.plugins.try_acquire_staged_op(claim.operation_id());
        Ok(OpBegin::Proceed((claim, guard)))
    })
    .await
}

fn validate_enabling_snapshot_sync(
    service_state: &ApiState,
    validation_host: &plugin_host::PluginHost,
    pid: &str,
    actor: UserId,
    snapshot: store::plugins::PluginActivationSnapshot,
) -> (
    store::plugins::PluginActivationSnapshot,
    Result<(), (&'static str, String)>,
) {
    let cfg = match validate_config_for_enable(
        service_state.store.as_ref(),
        &snapshot.plugin.plugin_id,
        &snapshot.version,
        snapshot.config.as_ref(),
    ) {
        Ok(cfg) => cfg,
        Err((reason, msg)) => return (snapshot, Err((reason, msg))),
    };
    let res = validate_installed_components(
        service_state,
        validation_host,
        pid,
        &snapshot.version,
        actor,
        Some(cfg),
    )
    .map_err(|e| ("validation-rejected", e.to_string()))
    .and_then(|()| {
        super::registry_sync::validate_enabling_plugin_providers(
            service_state,
            &snapshot.plugin,
            &snapshot.version,
        )
        .map_err(|e| ("registry-collision", e))
    });
    (snapshot, res)
}

impl PluginService {
    pub async fn enable_plugin(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: EnablePluginInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "enable",
        )
        .await?
        {
            return Ok(replayed);
        }

        let pid = plugin_id.to_string();
        let expected_rev = input.expected_revision;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key.clone();
        let req = input.request.clone();
        let st_guard = state.clone();
        let (snapshot, claim, _op_guard) = match run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: idem,
                request: req,
                kind: "enable".into(),
                expected_revision: Some(expected_rev),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("enable:{pid}")),
            })?;
            let op_id = accepted.operation.operation_id;
            if accepted.replayed {
                return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                    app, op_id,
                )?)));
            }
            let snapshot = app.begin_plugin_enable_checked(&pid, expected_rev, op_id)?;
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            let guard = st_guard.plugins.try_acquire_staged_op(op_id);
            Ok(OpBegin::Proceed((snapshot, claim, guard)))
        })
        .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(triple) => triple,
        };

        let service_state = state.clone();
        let pid = plugin_id.to_string();
        let validation_host = self
            .validation_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        let op_id = claim.operation_id();
        run_blocking(move || {
            let _guard = _op_guard;
            let (snapshot, res) = validate_enabling_snapshot_sync(
                &service_state,
                &validation_host,
                &pid,
                actor,
                snapshot,
            );
            let (succeeded, phase, fail_reason) = match &res {
                Ok(()) => (true, "enabled", None),
                Err((reason, _)) => (false, *reason, Some(*reason)),
            };
            let enabling_gen = snapshot.plugin.generation;
            let activated = run_foreground_app_tx(&service_state, true, |app| {
                finalize_enable_in_store(
                    app,
                    &pid,
                    &snapshot,
                    &claim,
                    succeeded,
                    phase,
                    fail_reason,
                )
            })?;
            if activated {
                service_state
                    .plugins
                    .in_flight()
                    .close_dispatch(&pid, enabling_gen);
                let _ = sync_all_plugin_tasks(&service_state, &pid);
                if let Err(err) = super::registry_sync::sync_registry_snapshot(&service_state, true)
                {
                    tracing::error!(plugin_id = %pid, error = %err, "Failed to publish RegistrySnapshot after plugin enable");
                }
            } else {
                service_state
                    .plugins
                    .in_flight()
                    .close_dispatch(&pid, enabling_gen.saturating_add(1));
            }
            load_operation_admin_view(&service_state.store, op_id)
        })
        .await
    }

    pub async fn disable_plugin(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: DisablePluginInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "disable",
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
        if current.revision != input.expected_revision
            || current.lifecycle != PluginLifecycle::Enabled
        {
            if let Some(replayed) = check_management_replay_after_write(
                state,
                input.actor_user_id,
                Some(plugin_id),
                &input.idempotency_key,
                &input.request,
                "disable",
            )
            .await?
            {
                return Ok(replayed);
            }
            return Err(if current.revision != input.expected_revision {
                PluginServiceError::conflict("plugin.revision_conflict", "plugin revision mismatch")
            } else {
                PluginServiceError::conflict("plugin.conflict", "plugin is not enabled")
            });
        }

        let pid = plugin_id.to_string();
        let expected_rev = input.expected_revision;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key.clone();
        let req = input.request.clone();
        let st_guard = state.clone();
        let (draining, claim, _op_guard) = match run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: idem,
                request: req,
                kind: "disable".into(),
                expected_revision: Some(expected_rev),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("disable:{pid}")),
            })?;
            let op_id = accepted.operation.operation_id;
            if accepted.replayed {
                return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                    app, op_id,
                )?)));
            }
            let draining = app.begin_plugin_lifecycle(&pid, expected_rev, op_id, false)?;
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            let guard = st_guard.plugins.try_acquire_staged_op(op_id);
            Ok(OpBegin::Proceed((draining, claim, guard)))
        })
        .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(triple) => triple,
        };
        if draining.lifecycle == PluginLifecycle::Draining {
            self.in_flight()
                .close_dispatch(plugin_id, draining.generation);
        }

        let s = state.clone();
        let pid = plugin_id.to_string();
        let reason = input.reason.clone();
        let op_id = claim.operation_id();
        run_blocking(move || {
            let _guard = _op_guard;
            if let Err(err) =
                drain_plugin_sync(&s.plugins, &s, &pid, op_id, &draining, reason.as_deref())
            {
                settle_drain_failure(&s, &claim, &err);
                return Err(err);
            }
            super::op_settlement::complete_foreground_non_staged_op(&s, &claim, true, "disabled")?;
            super::registry_sync::sync_registry_snapshot(&s, true)?;
            load_operation_admin_view(&s.store, op_id)
        })
        .await
    }

    pub async fn uninstall_plugin(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: UninstallPluginInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "uninstall",
        )
        .await?
        {
            return Ok(replayed);
        }

        let current = match load_plugin_for_uninstall(state, plugin_id, &input).await? {
            Ok(p) => p,
            Err(replayed) => return Ok(replayed),
        };
        let drain_rev = match ensure_drained_for_uninstall(
            self,
            state,
            plugin_id,
            &current,
            input.actor_user_id,
        )
        .await
        {
            Ok(rev) => rev,
            Err(err) => {
                if let Some(replayed) = check_management_replay_after_write(
                    state,
                    input.actor_user_id,
                    Some(plugin_id),
                    &input.idempotency_key,
                    &input.request,
                    "uninstall",
                )
                .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        let (claim, _active_op_guard) =
            match begin_staged_uninstall_claim(state, plugin_id, &input, drain_rev).await? {
                OpBegin::Replayed(view) => return Ok(*view),
                OpBegin::Proceed(pair) => pair,
            };
        let op_id = claim.operation_id();
        let s = state.clone();
        let pid = plugin_id.to_string();
        let remove_private = input.remove_private_data;
        run_blocking(move || {
            let _guard = _active_op_guard;
            let res = execute_staged_uninstall(&s, &claim, &pid, drain_rev, remove_private);
            if res
                .as_ref()
                .is_err_and(|e| !super::recovery::is_transient_or_admission_err(e))
            {
                let _ = super::op_settlement::settle_failed_staged_operation_mode(
                    &s,
                    op_id,
                    "uninstall-failed",
                    true,
                );
            }
            res
        })
        .await
    }
}

async fn ensure_drained_for_uninstall(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    current: &PluginRecord,
    actor: UserId,
) -> Result<i64, PluginServiceError> {
    if current.lifecycle == PluginLifecycle::Enabled {
        let pid = plugin_id.to_string();
        let rev = current.revision;
        let st_guard = state.clone();
        let (draining, claim, _op_guard) = run_app_write_tx(state, move |app| {
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: format!("uninstall-drain:{pid}:{rev}:{}", Uuid::new_v4()),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid}/disable"),
                    body_digest: [0; 32],
                },
                kind: "disable".into(),
                expected_revision: Some(rev),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("uninstall-drain:{pid}")),
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
            super::op_settlement::complete_foreground_non_staged_op(&s, &claim, true, "disabled")?;
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
        Ok(current.revision)
    } else {
        Err(PluginServiceError::conflict(
            "plugin.not_drained",
            "plugin is currently enabling or draining",
        ))
    }
}

fn execute_staged_uninstall(
    s: &ApiState,
    claim: &PluginCommandClaim,
    pid: &str,
    drain_rev: i64,
    remove_private: bool,
) -> Result<OperationAdminView, PluginServiceError> {
    let op_id = claim.operation_id();
    let (uninstalled, prev_gen, view) = super::service::run_foreground_app_tx(s, true, |app| {
        let prev_gen = app.get_plugin(pid)?.map_or(0, |p| p.generation);
        let uninstalled = app.uninstall_plugin(pid, drain_rev, Some(op_id), remove_private)?;
        let result = serde_json::json!({
            "plugin_id": pid,
            "private_data_removed": remove_private,
            "removed_tasks": uninstalled.removed_task_bindings.len(),
        });
        app.complete_staged_plugin_operation(
            claim,
            PluginOperationState::Succeeded,
            "uninstalled",
            Some(&result),
            None,
            now_unix(),
        )?;
        let view = load_app_operation_admin_view(app, op_id)?;
        Ok((uninstalled, prev_gen, view))
    })?;
    if !uninstalled.removed_task_bindings.is_empty()
        && let Ok(reservation) = s.jobs.reserve()
        && let Ok(queue) = reservation.acquire()
    {
        for binding in &uninstalled.removed_task_bindings {
            let _ = queue.delete_def(jobs::JobIdentity::plugin_task(
                PluginTaskBindingId::from_uuid(binding.binding_id),
            ));
        }
    }
    s.plugins.cache().evict_plugin(pid);
    s.plugins
        .in_flight()
        .on_uninstall(pid, prev_gen.saturating_add(1));
    let _ = super::registry_sync::sync_registry_snapshot(s, true);
    let _ = gc_unreferenced_artifacts_with_store(&s.store, s.plugins.staging_pins());
    Ok(view)
}
