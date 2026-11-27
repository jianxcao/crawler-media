use domain::PluginOperationId;
use store::StoreError;
use store::plugins::{
    PluginError, PluginLifecycle, PluginLifecycleObservation, PluginOperation,
    PluginOperationState, PluginReconciliationObservation,
};

use super::commands::{needs_reconcile, project_command_job, project_reconcile_job};
use super::errors::PluginServiceError;
use super::lifecycle_ops::{has_active_plugin_job, sync_all_plugin_tasks};
use super::recovery::settle_interrupted_lifecycle_operation;
use super::tasks::{is_plugin_active_for_binding, sync_task_binding};
use crate::jobs_writer::JobsMutationError;
use crate::management::ApiState;

const PAGE_SIZE: usize = 64;

const FLOOR_PREFIX: &str = "plugin.generation_floor.";

pub fn recover_startup(state: &ApiState) -> Result<(), PluginServiceError> {
    seed_in_flight_generation_floors(state)?;
    super::recovery::recover_all_staged_operations(state)?;
    seed_in_flight_generation_floors(state)?;
    recover_task_syncs(state, false)?;
    recover_task_syncs(state, true)?;
    interrupt_running_commands(state)?;
    let _ =
        crate::download_service::reconcile_pending_operations(state, crate::job_loop::unix_now());
    reconcile_pending(state, crate::job_loop::unix_now())?;
    super::registry_sync::sync_registry_snapshot(state, false)?;
    Ok(())
}

fn seed_in_flight_generation_floors(state: &ApiState) -> Result<(), PluginServiceError> {
    for key in state.store.list_setting_keys_with_prefix(FLOOR_PREFIX)? {
        if let Some(pid) = key.strip_prefix(FLOOR_PREFIX)
            && !pid.is_empty()
            && let Some(raw) = state.store.get_setting(&key)?
            && let Ok(floor) = raw.parse::<i64>()
            && floor > 0
        {
            state.plugins.in_flight().close_dispatch(pid, floor);
        }
    }
    for plugin in super::PluginService::list_all_plugins(&state.store, None)? {
        let min_open = if plugin.lifecycle == PluginLifecycle::Enabled && plugin.desired_enabled {
            plugin.generation
        } else {
            plugin.generation.saturating_add(1)
        };
        state
            .plugins
            .in_flight()
            .close_dispatch(&plugin.plugin_id, min_open);
    }
    Ok(())
}

fn recover_task_syncs(state: &ApiState, active_only: bool) -> Result<(), PluginServiceError> {
    let mut after = None;
    loop {
        let batch = state
            .store
            .pending_plugin_task_sync(after, Some(PAGE_SIZE))?;
        let next_after = batch.last().map(|b| b.binding_id);
        let len = batch.len();
        for binding in batch {
            let active = is_plugin_active_for_binding(state, &binding)?;
            if active == active_only {
                sync_task_binding(state, &binding)?;
            }
        }
        if len < PAGE_SIZE || next_after == after {
            break;
        }
        after = next_after;
    }
    Ok(())
}

fn interrupt_running_commands(state: &ApiState) -> Result<(), PluginServiceError> {
    let mut after = None;
    loop {
        let batch = state
            .store
            .unresolved_plugin_commands(after, Some(PAGE_SIZE))?;
        let next_after = batch.last().map(|p| p.operation.operation_id);
        let len = batch.len();
        for pending in batch {
            if pending.operation.state != PluginOperationState::Running
                || pending.operation.generation.is_none()
            {
                continue;
            }
            match state.store.record_plugin_command_interruption(
                pending.operation.operation_id,
                pending.operation.revision,
                pending.operation.generation,
                pending.attempt,
            ) {
                Ok(_)
                | Err(StoreError::Plugin(
                    PluginError::Conflict | PluginError::EffectDispositionRequired,
                )) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if len < PAGE_SIZE || next_after == after {
            break;
        }
        after = next_after;
    }
    Ok(())
}

pub fn reconcile_pending(state: &ApiState, now: i64) -> Result<(), PluginServiceError> {
    if !state.instance_writable() || state.write_gate.is_closed() {
        return Ok(());
    }
    reconcile_drain_effects(state, now)?;
    reconcile_enabling_plugins_step(state)?;
    reconcile_draining_plugins_step(state)?;
    Ok(())
}

pub(crate) fn reconcile_drain_effects(
    state: &ApiState,
    now: i64,
) -> Result<(), PluginServiceError> {
    if !state.instance_writable() || state.write_gate.is_closed() {
        return Ok(());
    }
    let _ = crate::download_service::reconcile_pending_operations(state, now);
    reconcile_task_step(state)?;
    reconcile_pending_commands_step(state)?;
    reconcile_unresolved_step(state)?;
    Ok(())
}

fn reconcile_enabling_plugins_step(state: &ApiState) -> Result<(), PluginServiceError> {
    let items =
        super::PluginService::list_all_plugins(&state.store, Some(PluginLifecycle::Enabling))?;
    let mut any_finalized = false;
    for plugin in items {
        let Some(op_id) = plugin.active_operation_id else {
            continue;
        };
        if state.plugins.is_op_active_in_process(op_id) {
            continue;
        }
        let obs = PluginLifecycleObservation {
            plugin_id: plugin.plugin_id.clone(),
            operation_id: op_id,
            revision: plugin.revision,
            generation: plugin.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: false,
        };
        let finalized = state.store.app_write(|app| {
            if state.plugins.is_op_active_in_process(op_id) {
                return Ok(false);
            }
            match app.finalize_plugin_lifecycle_checked(&obs, Some("lifecycle-interrupted")) {
                Ok(_) => Ok(true),
                Err(StoreError::Plugin(PluginError::Conflict | PluginError::NotDrained)) => {
                    Ok(false)
                }
                Err(err) => Err(err),
            }
        })?;
        if finalized {
            any_finalized = true;
            state
                .plugins
                .in_flight()
                .close_dispatch(&plugin.plugin_id, plugin.generation.saturating_add(1));
            let _ = sync_all_plugin_tasks(state, &plugin.plugin_id);
            let _ = settle_interrupted_lifecycle_operation(
                &state.store,
                op_id,
                false,
                "lifecycle-interrupted",
            );
        }
    }
    if any_finalized {
        let _ = super::registry_sync::sync_registry_snapshot(state, true);
    }
    Ok(())
}

fn reconcile_draining_plugins_step(state: &ApiState) -> Result<(), PluginServiceError> {
    let items =
        super::PluginService::list_all_plugins(&state.store, Some(PluginLifecycle::Draining))?;
    let any_draining = !items.is_empty();
    let mut any_finalized = false;
    for plugin in items {
        let Some(op_id) = plugin.active_operation_id else {
            continue;
        };
        let admin = state.store.get_plugin_operation_admin(op_id)?;
        if admin
            .as_ref()
            .is_some_and(|a| a.operation.state == PluginOperationState::Running)
            && state.plugins.is_op_active_in_process(op_id)
        {
            continue;
        }
        let in_flight_idle = state.plugins.in_flight().active_count(&plugin.plugin_id) == 0;
        let runtime_idle = state
            .plugins
            .maybe_runtime()
            .is_none_or(|rt| rt.active_plugin_count(&plugin.plugin_id) == 0);
        let jobs_idle = !has_active_plugin_job(state, &plugin.plugin_id);
        if !in_flight_idle || !runtime_idle || !jobs_idle {
            continue;
        }
        if let Some(ref admin_op) = admin
            && admin_op.intent.is_some()
        {
            if state.plugins.is_op_active_in_process(op_id)
                || super::idempotency::has_active_resume_claim(&state.store, op_id)
            {
                continue;
            }
            let Some(_guard) = state.plugins.try_acquire_staged_op(op_id) else {
                continue;
            };
            if super::idempotency::has_active_resume_claim(&state.store, op_id) {
                continue;
            }
            super::recovery::recover_one_staged_operation(state, admin_op)?;
            any_finalized = true;
            continue;
        }
        let obs = PluginLifecycleObservation {
            plugin_id: plugin.plugin_id.clone(),
            operation_id: op_id,
            revision: plugin.revision,
            generation: plugin.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        };
        let finalized_admin = state.store.app_write(|app| {
            let cur_admin = app.get_plugin_operation_admin(op_id)?;
            if cur_admin
                .as_ref()
                .is_some_and(|a| a.operation.state == PluginOperationState::Running)
                && state.plugins.is_op_active_in_process(op_id)
            {
                return Ok(None);
            }
            match app.finalize_plugin_lifecycle_checked(&obs, None) {
                Ok(_) => Ok(Some(cur_admin)),
                Err(StoreError::Plugin(PluginError::Conflict | PluginError::NotDrained)) => {
                    Ok(None)
                }
                Err(err) => Err(err),
            }
        })?;
        if let Some(cur_admin) = finalized_admin {
            any_finalized = true;
            state
                .plugins
                .in_flight()
                .close_dispatch(&plugin.plugin_id, plugin.generation.saturating_add(1));
            let _ = sync_all_plugin_tasks(state, &plugin.plugin_id);
            if cur_admin.is_some_and(|a| a.intent.is_some()) {
                let _ = super::op_settlement::settle_failed_staged_operation_mode(
                    state,
                    op_id,
                    "recovery-aborted",
                    false,
                );
            } else {
                let _ =
                    settle_interrupted_lifecycle_operation(&state.store, op_id, true, "disabled");
            }
        }
    }
    if any_draining || any_finalized {
        let _ = super::registry_sync::sync_registry_snapshot(state, true);
    }
    Ok(())
}

fn reconcile_task_step(state: &ApiState) -> Result<(), PluginServiceError> {
    let after = *state.plugins.tasks_cursor().lock();
    let mut batch = state
        .store
        .pending_plugin_task_sync(after, Some(PAGE_SIZE))?;
    if batch.is_empty() && after.is_some() {
        batch = state
            .store
            .pending_plugin_task_sync(None, Some(PAGE_SIZE))?;
    }
    *state.plugins.tasks_cursor().lock() = batch.last().map(|b| b.binding_id);
    for binding in batch {
        if let Err(error) = sync_task_binding(state, &binding) {
            if matches!(error, PluginServiceError::Store(StoreError::WriteBusy)) {
                return Err(error);
            }
            tracing::error!(binding_id = %binding.binding_id, %error, "Plugin task sync remains pending");
        }
    }
    Ok(())
}

fn reconcile_pending_commands_step(state: &ApiState) -> Result<(), PluginServiceError> {
    let after = state.plugins.commands_cursor().lock().clone();
    let mut batch =
        state
            .store
            .pending_plugin_commands(after.as_ref(), Some(PAGE_SIZE), i64::MAX)?;
    if batch.is_empty() && after.is_some() {
        batch = state
            .store
            .pending_plugin_commands(None, Some(PAGE_SIZE), i64::MAX)?;
    }
    *state.plugins.commands_cursor().lock() = batch.last().map(|item| item.cursor.clone());
    for item in batch {
        if item.operation.generation.is_none() {
            continue;
        }
        if let Err(error) = project_command_job(state, &item.operation) {
            if matches!(error, PluginServiceError::Store(StoreError::WriteBusy)) {
                return Err(error);
            }
            tracing::error!(operation_id = %item.operation.operation_id, %error, "Plugin command projection remains pending");
        }
    }
    Ok(())
}

fn reconcile_unresolved_step(state: &ApiState) -> Result<(), PluginServiceError> {
    let after = *state.plugins.unresolved_cursor().lock();
    let mut batch = state
        .store
        .unresolved_plugin_commands(after, Some(PAGE_SIZE))?;
    if batch.is_empty() && after.is_some() {
        batch = state
            .store
            .unresolved_plugin_commands(None, Some(PAGE_SIZE))?;
    }
    *state.plugins.unresolved_cursor().lock() =
        batch.last().map(|item| item.operation.operation_id);
    for item in batch {
        if !needs_reconcile(&item.operation) {
            continue;
        }
        if let Err(error) = project_reconcile_job(state, &item.operation) {
            if matches!(error, PluginServiceError::Store(StoreError::WriteBusy)) {
                return Err(error);
            }
            tracing::error!(operation_id = %item.operation.operation_id, %error, "Plugin reconcile projection remains pending");
        }
    }
    Ok(())
}

pub(crate) fn reconcile_pending_in_loop(
    state: &ApiState,
    now: i64,
) -> Result<(), JobsMutationError> {
    match reconcile_pending(state, now) {
        Ok(()) => Ok(()),
        Err(PluginServiceError::Store(e)) => Err(JobsMutationError::Acquire(e)),
        Err(PluginServiceError::Jobs(e)) => Err(JobsMutationError::Queue(e)),
        Err(other) => Err(JobsMutationError::Queue(jobs::JobError::Identity(
            other.to_string(),
        ))),
    }
}

pub fn reconcile_operation(
    state: &ApiState,
    op_id: PluginOperationId,
) -> Result<Option<PluginOperation>, String> {
    let pending = state
        .store
        .plugin_command_status(op_id.as_uuid())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("plugin operation {op_id} not found"))?;
    if pending.operation.state.is_terminal() || !needs_reconcile(&pending.operation) {
        return Ok(Some(pending.operation));
    }
    if pending.operation.state == PluginOperationState::Unknown
        && pending.operation.phase == "cancel-requested"
        && let Some(admin) = state
            .store
            .get_plugin_operation_admin(op_id.as_uuid())
            .map_err(|e| e.to_string())?
        && admin.intent.is_some()
    {
        super::recovery::reconcile_cancel_requested_staged_op(state, &admin)
            .map_err(|e| e.to_string())?;
        return state
            .store
            .plugin_command_status(op_id.as_uuid())
            .map_err(|e| e.to_string())
            .map(|p| p.map(|x| x.operation));
    }
    let outcome = state
        .plugins
        .effects()
        .reconcile(op_id, &pending.operation.kind, pending.attempt)
        .or_else(|| {
            matches!(
                pending.operation.kind.as_str(),
                "downloader.add"
                    | "downloader.control"
                    | "downloader.delete-task"
                    | "downloader.prepare-cleanup"
                    | "downloader.delete-files"
            )
            .then(|| {
                crate::download_service::reconcile_plugin_command_step(
                    state,
                    op_id,
                    &pending.operation.kind,
                    pending.attempt,
                    &pending.targets,
                )
            })
            .flatten()
        });
    let Some(outcome) = outcome else {
        return Ok(None);
    };
    match state
        .store
        .reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: op_id.as_uuid(),
            revision: pending.operation.revision,
            generation: pending.operation.generation,
            attempt: pending.attempt,
            outcome,
        }) {
        Ok(updated) => Ok(Some(updated)),
        Err(StoreError::Plugin(PluginError::Conflict)) => Ok(state
            .store
            .plugin_command_status(op_id.as_uuid())
            .map_err(|e| e.to_string())?
            .map(|p| p.operation)),
        Err(error) => Err(error.to_string()),
    }
}

pub fn run_reconcile_job(state: &ApiState, job: &jobs::Job) -> Result<(), String> {
    let jobs::JobScope::PluginOperation(op_id) = job.scope else {
        return Err("PluginReconcile requires PluginOperation scope".into());
    };
    match reconcile_operation(state, op_id)? {
        Some(_) => Ok(()),
        None => Err(format!(
            "plugin operation {op_id} effect remains unknown; awaiting reconciliation"
        )),
    }
}
