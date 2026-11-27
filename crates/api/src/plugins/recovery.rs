use std::path::Path;

use domain::UserId;
use plugin_package::validate_cmplugin_bytes;
use store::StoreError;
use store::plugins::{
    PluginAdminOperation, PluginError, PluginLifecycle, PluginOperationState,
    PluginReconciliationObservation, PluginReconciliationOutcome, PluginRecord,
};

use super::idempotency::{ResumeIdempotencyCtx, settle_resume_idempotency_in_app};
use super::install_ops::{seed_manifest_task_bindings_in_app, validate_grant_targets};
use super::lifecycle_ops::drain_plugin_sync_mode;
use super::reauth::now_unix;
use super::recovery_lifecycle::recover_interrupted_lifecycle_plugins;
pub(crate) use super::recovery_lifecycle::settle_interrupted_lifecycle_operation;
use super::recovery_rollforward::try_roll_forward_preserve_upgrade;
use super::service::run_foreground_app_tx;
use super::staging::{
    build_package_policy_from_store, gc_unreferenced_artifacts_with_store,
    materialize_immutable_package_sync, read_packaged_archive_for_version,
    version_metadata_from_validated,
};
use super::types::{PluginServiceError, map_package_err};
use super::wasm_runner::compile_validated_package_components;
use crate::management::ApiState;

pub(crate) fn recover_all_staged_operations(state: &ApiState) -> Result<(), PluginServiceError> {
    let mut after = None;
    loop {
        let page = state
            .store
            .list_unresolved_staged_plugin_operations(after, Some(64))?;
        let next = page.next_cursor.as_deref().and_then(|s| s.parse().ok());
        for item in page.items {
            if let Err(error) = recover_one_staged_operation(state, &item) {
                tracing::warn!(
                    operation_id = %item.operation.operation_id,
                    kind = %item.operation.kind,
                    %error,
                    "Staged plugin operation recovery encountered error"
                );
            }
        }
        match next {
            Some(cursor) if Some(cursor) != after => after = Some(cursor),
            _ => break,
        }
    }
    recover_interrupted_lifecycle_plugins(state)?;
    let _ = gc_unreferenced_artifacts_with_store(&state.store, state.plugins.staging_pins());
    Ok(())
}

pub(crate) fn recover_one_staged_operation(
    state: &ApiState,
    admin_op: &PluginAdminOperation,
) -> Result<(), PluginServiceError> {
    let op = &admin_op.operation;
    if op.state.is_terminal() {
        return Ok(());
    }
    if op.phase == "cancel-requested" {
        if op.state == PluginOperationState::Unknown {
            reconcile_cancel_requested_staged_op(state, admin_op)?;
        }
        return Ok(());
    }

    let now = now_unix();
    let mut claim = match op.state {
        PluginOperationState::Queued | PluginOperationState::Deferred => state
            .store
            .claim_staged_plugin_operation(op.operation_id, op.revision, admin_op.attempt, now)?,
        PluginOperationState::Running | PluginOperationState::Unknown => {
            state.store.recover_staged_plugin_operation(
                op.operation_id,
                op.revision,
                admin_op.attempt,
                "resuming",
                now,
            )?
        }
        PluginOperationState::Blocked if op.phase == "drain-pending" => {
            state.store.recover_staged_plugin_operation(
                op.operation_id,
                op.revision,
                admin_op.attempt,
                "resuming",
                now,
            )?
        }
        _ => return Ok(()),
    };

    let Some(intent) = &admin_op.intent else {
        let op_id = op.operation_id;
        super::config_ops::app_write_service(&state.store, |app| {
            app.complete_staged_plugin_operation(
                &claim,
                PluginOperationState::Failed,
                "missing-intent",
                None,
                Some("missing-intent"),
                now,
            )?;
            settle_resume_idempotency_in_app(app, op_id, None)?;
            Ok(())
        })?;
        return Ok(());
    };

    recover_claimed_staged_operation(state, &op.kind, &mut claim, intent, None, now, false, None)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn recover_claimed_staged_operation(
    state: &ApiState,
    op_kind: &str,
    claim: &mut store::plugins::PluginCommandClaim,
    intent: &store::plugins::StagedPluginIntent,
    actor: Option<UserId>,
    now: i64,
    foreground: bool,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<(), PluginServiceError> {
    let op_id = claim.operation_id();
    match op_kind {
        "install" => recover_staged_install(state, claim, intent, now, foreground, resume_ctx),
        "uninstall" | "install-prepare" | "upgrade-prepare" | "upgrade" | "rollback"
        | "state-reset" => {
            if let Some(plugin) = state.store.get_plugin(&intent.target_plugin_id)?
                && plugin.lifecycle == PluginLifecycle::Draining
                && plugin.active_operation_id == Some(op_id)
            {
                match drain_plugin_sync_mode(
                    &state.plugins,
                    state,
                    &plugin.plugin_id,
                    op_id,
                    &plugin,
                    Some("recovery-aborted"),
                    foreground,
                ) {
                    Ok(_) => {
                        let _ = super::registry_sync::sync_registry_snapshot(state, true);
                    }
                    Err(err) if is_transient_or_admission_err(&err) => return Err(err),
                    Err(_) => {
                        let saved_claim = claim.clone();
                        let res = run_foreground_app_tx(state, foreground, |app| {
                            app.record_plugin_command_outcome(
                                claim,
                                &store::plugins::PluginCommandOutcome {
                                    state: PluginOperationState::Blocked,
                                    phase: "drain-pending".into(),
                                    next_run_at: None,
                                    result: None,
                                    effect: store::plugins::PluginEffectObservation::NoEffect,
                                },
                            )?;
                            settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
                            Ok(())
                        });
                        if res.is_err() {
                            *claim = saved_claim;
                        }
                        return res;
                    }
                }
            }
            if op_kind == "uninstall" {
                return recover_staged_uninstall(state, claim, intent, now, foreground, resume_ctx);
            }
            if op_kind == "upgrade" || op_kind == "rollback" {
                return recover_staged_upgrade_or_rollback(
                    state, claim, intent, actor, now, foreground, resume_ctx,
                );
            }
            complete_staged_with_resume_tx(
                state,
                foreground,
                claim,
                PluginOperationState::Failed,
                "recovery-aborted",
                None,
                Some("recovery-aborted"),
                now,
                resume_ctx,
            )
        }
        _ => run_foreground_app_tx(state, foreground, |app| {
            settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
            Ok(())
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_staged_with_resume_tx(
    state: &ApiState,
    foreground: bool,
    claim: &store::plugins::PluginCommandClaim,
    state_out: PluginOperationState,
    phase: &str,
    result: Option<&serde_json::Value>,
    failure_reason: Option<&str>,
    now: i64,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<(), PluginServiceError> {
    let op_id = claim.operation_id();
    run_foreground_app_tx(state, foreground, |app| {
        app.complete_staged_plugin_operation(claim, state_out, phase, result, failure_reason, now)?;
        settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
        Ok(())
    })
}

pub(super) fn is_transient_or_admission_err(err: &PluginServiceError) -> bool {
    matches!(
        err,
        PluginServiceError::Conflict {
            code: "instance.read_only",
            ..
        } | PluginServiceError::Busy { .. }
            | PluginServiceError::Store(
                StoreError::WriteBusy | StoreError::ReadBusy | StoreError::ReadDeadline
            )
    )
}

fn best_effort_seed_tasks_for_plugin_in_app(
    app: &store::App<'_>,
    plugin: &store::plugins::PluginRecord,
) -> Result<(), PluginServiceError> {
    if let Some(ver) = app.get_plugin_version(&plugin.plugin_id, &plugin.package_hash)?
        && ver.manifest_json != "{}"
        && let Ok(manifest) =
            serde_json::from_str::<plugin_package::PluginManifest>(&ver.manifest_json)
    {
        seed_manifest_task_bindings_in_app(app, plugin, &manifest)?;
    }
    Ok(())
}

struct CancelStagedResolution {
    outcome: PluginReconciliationOutcome,
    plugin_to_seed: Option<PluginRecord>,
    evict_cache_id: Option<String>,
    sync_registry: bool,
}

fn evaluate_cancel_requested_staged_op(
    state: &ApiState,
    admin_op: &PluginAdminOperation,
    foreground: bool,
) -> Result<CancelStagedResolution, PluginServiceError> {
    let op = &admin_op.operation;
    let mut res = CancelStagedResolution {
        outcome: PluginReconciliationOutcome::ConfirmedNoEffect,
        plugin_to_seed: None,
        evict_cache_id: None,
        sync_registry: false,
    };
    let Some(intent) = &admin_op.intent else {
        return Ok(res);
    };
    match op.kind.as_str() {
        "install" => {
            if let Some(plugin) = state.store.get_plugin(&intent.target_plugin_id)?
                && (plugin.active_operation_id == Some(op.operation_id)
                    || intent.package_hash.as_deref() == Some(plugin.package_hash.as_str()))
            {
                res.plugin_to_seed = Some(plugin);
                res.outcome = PluginReconciliationOutcome::Succeeded;
            }
        }
        "upgrade" | "rollback" => {
            if let Some(mut plugin) = state.store.get_plugin(&intent.target_plugin_id)? {
                if plugin.lifecycle == PluginLifecycle::Draining
                    && plugin.active_operation_id == Some(op.operation_id)
                {
                    if drain_plugin_sync_mode(
                        &state.plugins,
                        state,
                        &plugin.plugin_id,
                        op.operation_id,
                        &plugin,
                        Some("cancel-requested"),
                        foreground,
                    )
                    .is_ok()
                    {
                        let _ = super::registry_sync::sync_registry_snapshot(state, true);
                    }
                    if let Some(refreshed) = state.store.get_plugin(&plugin.plugin_id)? {
                        plugin = refreshed;
                    }
                }
                if intent.package_hash.as_deref() == Some(plugin.package_hash.as_str())
                    && matches!(
                        plugin.lifecycle,
                        PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
                    )
                {
                    res.evict_cache_id = Some(plugin.plugin_id.clone());
                    res.sync_registry = true;
                    res.plugin_to_seed = Some(plugin);
                    res.outcome = PluginReconciliationOutcome::Succeeded;
                }
            }
        }
        "uninstall" => match state.store.get_plugin(&intent.target_plugin_id)? {
            None => {
                res.evict_cache_id = Some(intent.target_plugin_id.clone());
                res.sync_registry = true;
                res.outcome = PluginReconciliationOutcome::Succeeded;
            }
            Some(plugin) => {
                if plugin.lifecycle == PluginLifecycle::Draining
                    && plugin.active_operation_id == Some(op.operation_id)
                    && drain_plugin_sync_mode(
                        &state.plugins,
                        state,
                        &plugin.plugin_id,
                        op.operation_id,
                        &plugin,
                        Some("cancel-requested"),
                        foreground,
                    )
                    .is_ok()
                {
                    let _ = super::registry_sync::sync_registry_snapshot(state, true);
                }
            }
        },
        _ => {}
    }
    Ok(res)
}

pub(super) fn reconcile_cancel_requested_staged_op(
    state: &ApiState,
    admin_op: &PluginAdminOperation,
) -> Result<(), PluginServiceError> {
    reconcile_cancel_requested_staged_op_mode(state, admin_op, false)
}

pub(super) fn reconcile_cancel_requested_staged_op_mode(
    state: &ApiState,
    admin_op: &PluginAdminOperation,
    foreground: bool,
) -> Result<(), PluginServiceError> {
    let op = &admin_op.operation;
    let res = evaluate_cancel_requested_staged_op(state, admin_op, foreground)?;
    run_foreground_app_tx(state, foreground, |app| {
        if let Some(ref plugin) = res.plugin_to_seed {
            let _ = best_effort_seed_tasks_for_plugin_in_app(app, plugin);
        }
        match app.reconcile_plugin_command(&PluginReconciliationObservation {
            operation_id: op.operation_id,
            revision: op.revision,
            generation: op.generation,
            attempt: admin_op.attempt,
            outcome: res.outcome,
        }) {
            Ok(_) | Err(StoreError::Plugin(PluginError::Conflict)) => {}
            Err(err) => return Err(err.into()),
        }
        settle_resume_idempotency_in_app(app, op.operation_id, None)?;
        Ok(())
    })?;
    if let Some(ref plugin) = res.plugin_to_seed {
        state
            .plugins
            .in_flight()
            .close_dispatch(&plugin.plugin_id, plugin.generation.saturating_add(1));
    }
    if let Some(pid) = res.evict_cache_id {
        state.plugins.cache().evict_plugin(&pid);
    }
    if res.sync_registry {
        let _ = super::registry_sync::sync_registry_snapshot(state, true);
    }
    Ok(())
}

pub(super) fn read_recovery_archive(
    data_dir: &Path,
    artifact_path: Option<&str>,
    plugin_id: &str,
    package_hash: Option<&str>,
    state: &ApiState,
) -> Result<(Vec<u8>, Option<String>), PluginServiceError> {
    if let Some(rel) = artifact_path {
        let p = Path::new(rel);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            data_dir.join(p)
        };
        if full.is_file() {
            let bytes = std::fs::read(&full).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            let sig_path = full.with_extension("minisig");
            let sig = if sig_path.exists() {
                std::fs::read_to_string(&sig_path).ok()
            } else {
                None
            };
            return Ok((bytes, sig));
        }
    }
    if let Some(hash) = package_hash
        && let Some(ver) = state.store.get_plugin_version(plugin_id, hash)?
    {
        return read_packaged_archive_for_version(data_dir, &ver);
    }
    Err(PluginServiceError::not_found(
        "plugin.artifact_missing",
        "staged archive unavailable for recovery",
    ))
}

fn recover_staged_install(
    state: &ApiState,
    claim: &mut store::plugins::PluginCommandClaim,
    intent: &store::plugins::StagedPluginIntent,
    now: i64,
    foreground: bool,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<(), PluginServiceError> {
    let Some(expected_hash) = intent.package_hash.as_deref() else {
        return complete_staged_with_resume_tx(
            state,
            foreground,
            claim,
            PluginOperationState::Failed,
            "recovery-failed",
            None,
            Some("missing-package-hash"),
            now,
            resume_ctx,
        );
    };
    let pin = state.plugins.staging_pins().pin(expected_hash);
    let res = (|| -> Result<(), PluginServiceError> {
        let (bytes, sig) = read_recovery_archive(
            state.store.data_dir(),
            intent.artifact_path.as_deref(),
            &intent.target_plugin_id,
            Some(expected_hash),
            state,
        )?;
        let policy = build_package_policy_from_store(&state.store, sig.clone())?;
        let validated = validate_cmplugin_bytes(&bytes, &policy).map_err(map_package_err)?;
        if validated.package_sha256_hex != expected_hash
            || validated.manifest.id != intent.target_plugin_id
        {
            return Err(PluginServiceError::conflict(
                "plugin.package_conflict",
                "recovered package digest/id mismatch",
            ));
        }
        let host = state
            .plugins
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;
        compile_validated_package_components(host, &validated)
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;
        let artifact_rel = materialize_immutable_package_sync(
            state.store.data_dir(),
            &bytes,
            sig.as_deref(),
            &validated,
        )?;
        let version_meta = version_metadata_from_validated(&validated, artifact_rel);
        let op_id = claim.operation_id();
        let saved_claim = claim.clone();
        let committed = run_foreground_app_tx(state, foreground, |app| {
            validate_grant_targets(app, &intent.grants)?;
            let record =
                app.register_staged_plugin_version(claim, &version_meta, &intent.grants, now)?;
            seed_manifest_task_bindings_in_app(app, &record, &validated.manifest)?;
            let result = serde_json::json!({
                "plugin_id": record.plugin_id,
                "package_hash": record.package_hash,
                "recovered": true,
            });
            app.complete_staged_plugin_operation(
                claim,
                PluginOperationState::Succeeded,
                "installed",
                Some(&result),
                None,
                now,
            )?;
            settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
            Ok(record.generation)
        });
        match committed {
            Ok(installed_gen) => {
                state
                    .plugins
                    .in_flight()
                    .close_dispatch(&intent.target_plugin_id, installed_gen.saturating_add(1));
                Ok(())
            }
            Err(err) => {
                *claim = saved_claim;
                Err(err)
            }
        }
    })();
    drop(pin);

    if let Err(err) = res {
        if is_transient_or_admission_err(&err) {
            return Err(err);
        }
        complete_staged_with_resume_tx(
            state,
            foreground,
            claim,
            PluginOperationState::Failed,
            "recovery-failed",
            None,
            Some("recovery-failed"),
            now,
            resume_ctx,
        )?;
        let _ = gc_unreferenced_artifacts_with_store(&state.store, state.plugins.staging_pins());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn recover_staged_upgrade_or_rollback(
    state: &ApiState,
    claim: &mut store::plugins::PluginCommandClaim,
    intent: &store::plugins::StagedPluginIntent,
    actor: Option<UserId>,
    now: i64,
    foreground: bool,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<(), PluginServiceError> {
    let Some(plugin) = state.store.get_plugin(&intent.target_plugin_id)? else {
        return complete_staged_with_resume_tx(
            state,
            foreground,
            claim,
            PluginOperationState::Failed,
            "recovery-failed",
            None,
            Some("plugin-missing"),
            now,
            resume_ctx,
        );
    };
    if let Some(target_hash) = &intent.package_hash
        && &plugin.package_hash == target_hash
        && matches!(
            plugin.lifecycle,
            PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
        )
    {
        let op_id = claim.operation_id();
        let result = serde_json::json!({
            "plugin_id": plugin.plugin_id,
            "package_hash": plugin.package_hash,
            "recovered": true,
        });
        run_foreground_app_tx(state, foreground, |app| {
            let _ = best_effort_seed_tasks_for_plugin_in_app(app, &plugin);
            app.complete_staged_plugin_operation(
                claim,
                PluginOperationState::Succeeded,
                "switched",
                Some(&result),
                None,
                now,
            )?;
            settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
            Ok(())
        })?;
        state.plugins.cache().evict_plugin(&plugin.plugin_id);
        state
            .plugins
            .in_flight()
            .close_dispatch(&plugin.plugin_id, plugin.generation.saturating_add(1));
        let _ = super::registry_sync::sync_registry_snapshot(state, true);
        return Ok(());
    }
    let roll_actor = actor.unwrap_or_default();
    if intent.state_policy.as_deref() == Some("preserve")
        && let Some(target_hash) = intent.package_hash.as_deref()
        && try_roll_forward_preserve_upgrade(
            state,
            claim,
            &plugin,
            intent,
            target_hash,
            roll_actor,
            now,
            foreground,
            resume_ctx,
        )?
    {
        return Ok(());
    }

    complete_staged_with_resume_tx(
        state,
        foreground,
        claim,
        PluginOperationState::Failed,
        "recovery-aborted",
        None,
        Some("recovery-aborted"),
        now,
        resume_ctx,
    )?;
    let _ = super::staging::gc_unreferenced_artifacts_with_store(
        &state.store,
        state.plugins.staging_pins(),
    );
    Ok(())
}

fn recover_staged_uninstall(
    state: &ApiState,
    claim: &store::plugins::PluginCommandClaim,
    intent: &store::plugins::StagedPluginIntent,
    now: i64,
    foreground: bool,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<(), PluginServiceError> {
    let remove_private = intent.state_policy.as_deref() == Some("remove");
    let op_id = claim.operation_id();
    let uninstalled = run_foreground_app_tx(state, foreground, |app| {
        let mut removed = Vec::new();
        let mut prev_gen = 0i64;
        if let Some(plugin) = app.get_plugin(&intent.target_plugin_id)? {
            if !matches!(
                plugin.lifecycle,
                PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
            ) {
                app.complete_staged_plugin_operation(
                    claim,
                    PluginOperationState::Failed,
                    "recovery-aborted",
                    None,
                    Some("not-drained"),
                    now,
                )?;
                settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
                return Ok(None);
            }
            prev_gen = plugin.generation;
            let res = match app.uninstall_plugin(
                &plugin.plugin_id,
                plugin.revision,
                Some(op_id),
                remove_private,
            ) {
                Ok(r) => r,
                Err(StoreError::Plugin(PluginError::NotDrained)) => {
                    app.complete_staged_plugin_operation(
                        claim,
                        PluginOperationState::Failed,
                        "recovery-aborted",
                        None,
                        Some("not-drained"),
                        now,
                    )?;
                    settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
                    return Ok(None);
                }
                Err(err) => return Err(err.into()),
            };
            removed = res.removed_task_bindings;
        }
        let result = serde_json::json!({
            "plugin_id": intent.target_plugin_id,
            "private_data_removed": remove_private,
            "removed_tasks": removed.len(),
        });
        app.complete_staged_plugin_operation(
            claim,
            PluginOperationState::Succeeded,
            "uninstalled",
            Some(&result),
            None,
            now,
        )?;
        settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
        Ok(Some((removed, prev_gen)))
    })?;
    let Some((removed_bindings, prev_gen)) = uninstalled else {
        return Ok(());
    };
    if !removed_bindings.is_empty()
        && let Ok(reservation) = state.jobs.reserve()
        && let Ok(queue) = reservation.acquire()
    {
        for b in &removed_bindings {
            let _ = queue.delete_def(jobs::JobIdentity::plugin_task(
                domain::PluginTaskBindingId::from_uuid(b.binding_id),
            ));
        }
    }
    state.plugins.cache().evict_plugin(&intent.target_plugin_id);
    state
        .plugins
        .in_flight()
        .on_uninstall(&intent.target_plugin_id, prev_gen.saturating_add(1));
    let _ = super::registry_sync::sync_registry_snapshot(state, true);
    Ok(())
}
