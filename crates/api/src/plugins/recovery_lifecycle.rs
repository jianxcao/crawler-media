use store::plugins::{
    PluginLifecycle, PluginLifecycleObservation, PluginOperationState,
    PluginReconciliationObservation, PluginReconciliationOutcome,
};
use store::{Store, StoreError};
use uuid::Uuid;

use super::lifecycle_ops::{complete_non_staged_op, sync_all_plugin_tasks};
use super::reauth::now_unix;
use super::types::PluginServiceError;
use crate::management::ApiState;

pub(crate) fn settle_interrupted_lifecycle_operation(
    store: &Store,
    op_id: Uuid,
    succeeded: bool,
    phase: &str,
) -> Result<(), PluginServiceError> {
    let Some(admin) = store.get_plugin_operation_admin(op_id)? else {
        return Ok(());
    };
    if admin.intent.is_some() || admin.operation.state.is_terminal() {
        return Ok(());
    }
    let op = &admin.operation;
    match op.state {
        PluginOperationState::Queued | PluginOperationState::Deferred => {
            if let Ok(claim) = store.claim_plugin_command(
                op_id,
                op.revision,
                op.generation,
                admin.attempt,
                now_unix(),
            ) {
                let _ = complete_non_staged_op(store, &claim, succeeded, phase);
            }
        }
        PluginOperationState::Running => {
            if let Ok(interrupted) = store.record_plugin_command_interruption(
                op_id,
                op.revision,
                op.generation,
                admin.attempt,
            ) {
                let outcome = if succeeded {
                    PluginReconciliationOutcome::Succeeded
                } else {
                    PluginReconciliationOutcome::Failed
                };
                let _ = store.reconcile_plugin_command(&PluginReconciliationObservation {
                    operation_id: op_id,
                    revision: interrupted.revision,
                    generation: interrupted.generation,
                    attempt: admin.attempt,
                    outcome,
                });
            }
        }
        PluginOperationState::Unknown => {
            let outcome = if succeeded {
                PluginReconciliationOutcome::Succeeded
            } else {
                PluginReconciliationOutcome::Failed
            };
            let _ = store.reconcile_plugin_command(&PluginReconciliationObservation {
                operation_id: op_id,
                revision: op.revision,
                generation: op.generation,
                attempt: admin.attempt,
                outcome,
            });
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn recover_interrupted_lifecycle_plugins(
    state: &ApiState,
) -> Result<(), PluginServiceError> {
    let mut after: Option<String> = None;
    loop {
        let page = state
            .store
            .list_plugins_page(None, after.as_deref(), Some(200))?;
        for plugin in &page.items {
            if matches!(
                plugin.lifecycle,
                PluginLifecycle::Enabling | PluginLifecycle::Draining
            ) && let Some(op_id) = plugin.active_operation_id
            {
                let succeeded = plugin.lifecycle == PluginLifecycle::Draining;
                let obs = PluginLifecycleObservation {
                    plugin_id: plugin.plugin_id.clone(),
                    operation_id: op_id,
                    revision: plugin.revision,
                    generation: plugin.generation,
                    workers_exited: true,
                    unknown_effects_reconciled: true,
                    validation_succeeded: succeeded,
                };
                let reason = (!succeeded).then_some("startup-interrupted");
                match state.store.finalize_plugin_lifecycle_checked(&obs, reason) {
                    Ok(_) => {
                        state
                            .plugins
                            .in_flight()
                            .close_dispatch(&plugin.plugin_id, plugin.generation.saturating_add(1));
                        let _ = sync_all_plugin_tasks(state, &plugin.plugin_id);
                        let _ = super::registry_sync::sync_registry_snapshot(state, true);
                        let settle_phase = if succeeded {
                            "disabled"
                        } else {
                            "startup-interrupted"
                        };
                        let _ = settle_interrupted_lifecycle_operation(
                            &state.store,
                            op_id,
                            succeeded,
                            settle_phase,
                        );
                    }
                    Err(StoreError::Plugin(store::plugins::PluginError::NotDrained))
                        if plugin.lifecycle == PluginLifecycle::Draining =>
                    {
                        let _ = settle_interrupted_lifecycle_operation(
                            &state.store,
                            op_id,
                            false,
                            "not-drained",
                        );
                    }
                    Err(StoreError::Plugin(
                        store::plugins::PluginError::NotDrained
                        | store::plugins::PluginError::Conflict,
                    )) => {}
                    Err(err) => return Err(err.into()),
                }
            }
        }
        match page.next_cursor {
            Some(next) if Some(&next) != after.as_ref() => after = Some(next),
            _ => break,
        }
    }
    settle_orphaned_management_operations(state)
}

fn settle_orphaned_management_operations(state: &ApiState) -> Result<(), PluginServiceError> {
    let mut after = None;
    loop {
        let batch = state.store.unresolved_plugin_commands(after, Some(64))?;
        let next_after = batch.last().map(|p| p.operation.operation_id);
        let len = batch.len();
        for pending in batch {
            let op = &pending.operation;
            if op.generation.is_some()
                || !matches!(
                    op.kind.as_str(),
                    "enable" | "disable" | "revoke-permissions"
                )
            {
                continue;
            }
            if let Some(pid) = op.plugin_id.as_deref()
                && let Some(p) = state.store.get_plugin(pid)?
                && p.active_operation_id == Some(op.operation_id)
                && matches!(
                    p.lifecycle,
                    PluginLifecycle::Enabling | PluginLifecycle::Draining
                )
            {
                continue;
            }
            let _ = settle_interrupted_lifecycle_operation(
                &state.store,
                op.operation_id,
                false,
                "startup-interrupted",
            );
        }
        if len < 64 || next_after == after {
            break;
        }
        after = next_after;
    }
    Ok(())
}
