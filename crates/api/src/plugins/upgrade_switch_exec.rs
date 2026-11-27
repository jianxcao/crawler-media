use std::collections::{BTreeMap, BTreeSet};

use plugin_package::{SecretWriteAction, validate_cmplugin_bytes};
use store::plugins::{
    ApplyPluginPackageSwitch, PluginConfig, PluginError, PluginGrant, PluginLifecycle,
    PluginOperationState, PluginSecretChange, PluginStatePolicy, PluginUpgradeConfig,
};
use uuid::Uuid;

use super::config_ops::validate_config_object_grants;
use super::idempotency::load_app_operation_admin_view;
use super::install_ops::{seed_manifest_task_bindings_in_app, validate_grant_targets};
use super::reauth::now_unix;
use super::service::run_foreground_app_tx;
use super::staging::{
    build_package_policy_from_store, gc_unreferenced_artifacts_with_store,
    materialize_immutable_package_sync, read_packaged_archive_for_version,
    read_staged_upload_for_actor_sync, version_metadata_from_validated,
};
use super::types::{OperationAdminView, PluginServiceError, map_package_err};
use super::wasm_runner::{compile_validated_package_components, validate_installed_components};
use crate::management::ApiState;

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_package_switch_sync(
    s: &ApiState,
    host: &plugin_host::PluginHost,
    validation_host: &plugin_host::PluginHost,
    claim: &mut store::plugins::PluginCommandClaim,
    pid: &str,
    actor_user_id: domain::UserId,
    upload_id: Option<Uuid>,
    package_hash: &str,
    version: &str,
    state_policy_exec: PluginStatePolicy,
    target_config: Option<PluginUpgradeConfig>,
    grants_exec: Vec<PluginGrant>,
) -> Result<OperationAdminView, PluginServiceError> {
    let op_id = claim.operation_id();
    let cur = s
        .store
        .get_plugin(pid)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
    if !matches!(
        cur.lifecycle,
        PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
    ) {
        return Err(PluginServiceError::conflict(
            "plugin.not_drained",
            "plugin cannot switch packages while enabling or draining",
        ));
    }

    let (pkg_bytes, sig_text, validated) = load_and_validate_switch_package(
        s,
        host,
        pid,
        Some(actor_user_id),
        upload_id,
        package_hash,
    )?;

    let saved_phase_claim = claim.clone();
    if let Err(err) = run_foreground_app_tx(s, true, |app| {
        Ok(app.advance_staged_plugin_phase(claim, "switching", None, now_unix())?)
    }) {
        *claim = saved_phase_claim;
        return Err(err);
    }
    let (target_ver, need_register) = resolve_switch_target_version(
        s,
        pid,
        upload_id,
        package_hash,
        &pkg_bytes,
        sig_text.as_deref(),
        &validated,
    )?;
    let validation_cfg = match &target_config {
        Some(cfg) => Some(PluginConfig {
            schema_revision: cfg.schema_revision.clone(),
            revision: cur.config_revision + 1,
            values: cfg.values.clone(),
        }),
        None => s.store.get_plugin_config(pid)?,
    };
    validate_installed_components(
        s,
        validation_host,
        pid,
        &target_ver,
        actor_user_id,
        validation_cfg,
    )
    .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;

    let saved_claim = claim.clone();
    let commit_res = run_foreground_app_tx(s, true, |app| {
        commit_package_switch_in_app(
            app,
            claim,
            pid,
            cur.revision,
            op_id,
            package_hash,
            version,
            need_register,
            &target_ver,
            &validated,
            state_policy_exec,
            target_config,
            grants_exec,
        )
    });
    let (view, switched_gen) = match commit_res {
        Ok(pair) => pair,
        Err(err) => {
            *claim = saved_claim;
            return Err(err);
        }
    };

    finalize_switched_operation(s, pid, &cur.package_hash, switched_gen)?;
    Ok(view)
}

#[allow(clippy::too_many_arguments)]
fn commit_package_switch_in_app(
    app: &store::App<'_>,
    claim: &mut store::plugins::PluginCommandClaim,
    pid: &str,
    expected_revision: i64,
    op_id: Uuid,
    package_hash: &str,
    version: &str,
    need_register: bool,
    target_ver: &store::plugins::PluginVersionMetadata,
    validated: &plugin_package::ValidatedPackage,
    state_policy_exec: PluginStatePolicy,
    target_config: Option<PluginUpgradeConfig>,
    grants_exec: Vec<PluginGrant>,
) -> Result<(OperationAdminView, i64), PluginServiceError> {
    revalidate_switch_config_in_app(
        app,
        pid,
        target_ver.credential_bound,
        &validated.config_schema,
        target_config.as_ref(),
    )?;
    validate_grant_targets(app, &grants_exec)?;
    let now = now_unix();
    let reg = need_register.then_some((&mut *claim, target_ver, now));
    let switched = app.register_and_apply_plugin_package_switch(
        reg,
        &ApplyPluginPackageSwitch {
            plugin_id: pid.to_string(),
            expected_revision,
            operation_id: Some(op_id),
            target_package_hash: package_hash.to_string(),
            state_policy: state_policy_exec,
            target_config,
            target_grants: grants_exec,
        },
    )?;
    seed_manifest_task_bindings_in_app(app, &switched, &validated.manifest)?;
    let result = serde_json::json!({
        "plugin_id": switched.plugin_id,
        "package_hash": switched.package_hash,
        "version": version,
        "revision": switched.revision,
        "generation": switched.generation,
    });
    app.complete_staged_plugin_operation(
        claim,
        PluginOperationState::Succeeded,
        "switched",
        Some(&result),
        None,
        now,
    )?;
    let view = load_app_operation_admin_view(app, op_id)?;
    Ok((view, switched.generation))
}

fn finalize_switched_operation(
    s: &ApiState,
    pid: &str,
    old_hash: &str,
    switched_gen: i64,
) -> Result<(), PluginServiceError> {
    s.plugins
        .in_flight()
        .close_dispatch(pid, switched_gen.saturating_add(1));
    s.plugins.cache().evict_hash(pid, old_hash);
    s.plugins.cache().evict_plugin(pid);
    super::registry_sync::sync_registry_snapshot(s, true)?;
    let _ = gc_unreferenced_artifacts_with_store(&s.store, s.plugins.staging_pins());
    Ok(())
}

fn revalidate_switch_config_in_app(
    app: &store::App<'_>,
    pid: &str,
    credential_bound: bool,
    schema: &plugin_package::ValidatedConfigSchema,
    target_config: Option<&PluginUpgradeConfig>,
) -> Result<(), PluginServiceError> {
    let Some(cfg) = target_config else {
        return Ok(());
    };
    validate_config_object_grants(app, &cfg.values)?;
    if !cfg.secret_scope_updates.is_empty() {
        let summary = (!schema.secret_fields().is_empty())
            .then(|| app.get_plugin_config_summary(pid))
            .transpose()?;
        for (scope, updates) in &cfg.secret_scope_updates {
            if !super::config_ops::is_live_secret_scope(
                app,
                credential_bound,
                scope.site_id(),
                scope,
            )? {
                return Err(PluginServiceError::conflict(
                    "plugin.consent_stale",
                    "secret scope invalidated before package switch",
                ));
            }
            if let Some(ref sum) = summary {
                let mut actions = BTreeMap::new();
                for u in updates {
                    let act = match &u.change {
                        PluginSecretChange::Keep => SecretWriteAction::Keep,
                        PluginSecretChange::Replace(bytes) => {
                            let val = String::from_utf8(bytes.clone()).map_err(|_| {
                                PluginServiceError::conflict(
                                    "plugin.consent_stale",
                                    "invalid secret bytes in target_config",
                                )
                            })?;
                            SecretWriteAction::Replace(val)
                        }
                        PluginSecretChange::Clear => SecretWriteAction::Clear,
                    };
                    actions.insert(u.key.clone(), act);
                }
                let scope_live_keys: BTreeSet<String> = sum
                    .secrets
                    .iter()
                    .filter(|s| s.configured && &s.scope == scope)
                    .map(|s| s.key.clone())
                    .collect();
                if schema
                    .resolve_config_write(&cfg.values, &actions, &scope_live_keys)
                    .is_err()
                {
                    return Err(PluginServiceError::conflict(
                        "plugin.consent_stale",
                        "secret scope invalidated before package switch",
                    ));
                }
            }
        }
    } else if !schema.secret_fields().is_empty() {
        let summary = app.get_plugin_config_summary(pid)?;
        if super::config_ops::resolve_config_write_for_scopes(
            app,
            schema,
            &cfg.values,
            &BTreeMap::new(),
            &summary.secrets,
            credential_bound,
            None,
        )?
        .is_err()
        {
            return Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "secret scope invalidated before package switch",
            ));
        }
    }
    Ok(())
}

fn resolve_switch_target_version(
    s: &ApiState,
    pid: &str,
    upload_id: Option<Uuid>,
    package_hash: &str,
    pkg_bytes: &[u8],
    sig_text: Option<&str>,
    validated: &plugin_package::ValidatedPackage,
) -> Result<(store::plugins::PluginVersionMetadata, bool), PluginServiceError> {
    match s.store.get_plugin_version(pid, package_hash)? {
        Some(existing) => Ok((existing, false)),
        None if upload_id.is_some() => {
            let rel = materialize_immutable_package_sync(
                s.store.data_dir(),
                pkg_bytes,
                sig_text,
                validated,
            )?;
            Ok((version_metadata_from_validated(validated, rel), true))
        }
        None => Err(PluginServiceError::not_found(
            "plugin.version_missing",
            "target version missing",
        )),
    }
}

fn load_and_validate_switch_package(
    s: &ApiState,
    host: &plugin_host::PluginHost,
    pid: &str,
    expected_actor: Option<domain::UserId>,
    upload_id: Option<Uuid>,
    package_hash: &str,
) -> Result<(Vec<u8>, Option<String>, plugin_package::ValidatedPackage), PluginServiceError> {
    let (pkg_bytes, sig_text) = if let Some(uid) = upload_id {
        let (b, sig, _) =
            read_staged_upload_for_actor_sync(s.store.data_dir(), uid, expected_actor)?;
        (b, sig)
    } else {
        let hist = s
            .store
            .get_plugin_version(pid, package_hash)?
            .ok_or_else(|| {
                PluginServiceError::not_found(
                    "plugin.version_missing",
                    "historical package version missing",
                )
            })?;
        read_packaged_archive_for_version(s.store.data_dir(), &hist)?
    };
    let _ = PluginError::Conflict;
    let policy = build_package_policy_from_store(&s.store, sig_text.clone())?;
    let validated = validate_cmplugin_bytes(&pkg_bytes, &policy).map_err(map_package_err)?;
    if validated.package_sha256_hex != package_hash {
        return Err(PluginServiceError::conflict(
            "plugin.package_conflict",
            "target package hash mismatch",
        ));
    }
    compile_validated_package_components(host, &validated)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;
    Ok((pkg_bytes, sig_text, validated))
}
