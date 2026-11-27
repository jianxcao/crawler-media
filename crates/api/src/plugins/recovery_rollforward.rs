use std::collections::BTreeMap;

use domain::UserId;
use plugin_package::validate_cmplugin_bytes;
use sha2::{Digest, Sha256};
use store::plugins::{
    ApplyPluginPackageSwitch, PluginConfig, PluginError, PluginLifecycle, PluginOperationState,
    PluginStatePolicy, PluginUpgradeConfig, PluginVersionMetadata,
};

use super::config_ops::{PluginConfigLookup, validate_config_object_grants};
use super::idempotency::{ResumeIdempotencyCtx, settle_resume_idempotency_in_app};
use super::install_ops::{seed_manifest_task_bindings_in_app, validate_grant_targets};
use super::recovery::{is_transient_or_admission_err, read_recovery_archive};
use super::service::run_foreground_app_tx;
use super::staging::{
    build_package_policy_from_store, hex_lower, materialize_immutable_package_sync,
    version_metadata_from_validated,
};
use super::types::PluginServiceError;
use super::wasm_runner::{compile_validated_package_components, validate_installed_components};
use crate::management::ApiState;

struct PreparedRollforward {
    validated: plugin_package::ValidatedPackage,
    target_ver: PluginVersionMetadata,
    need_register: bool,
}

fn verify_preserve_rollforward_config<L: PluginConfigLookup>(
    lookup: &L,
    plugin_id: &str,
    validated: &plugin_package::ValidatedPackage,
    expected_digest: Option<&str>,
) -> Result<Option<(Option<PluginUpgradeConfig>, PluginConfig)>, PluginServiceError> {
    let existing_cfg = lookup.get_plugin_config(plugin_id)?;
    let summary = lookup.get_plugin_config_summary(plugin_id)?;
    let raw_values = existing_cfg
        .as_ref()
        .map(|c| c.values.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    let Ok(materialized) = validated
        .config_schema
        .validate_values(&raw_values, existing_cfg.is_none())
    else {
        return Ok(None);
    };
    let Ok(resolved) = super::config_ops::resolve_config_write_for_scopes(
        lookup,
        &validated.config_schema,
        &materialized,
        &BTreeMap::new(),
        &summary.secrets,
        validated.credential_bound,
        None,
    )?
    else {
        return Ok(None);
    };
    if validate_config_object_grants(lookup, &resolved.public_values).is_err() {
        return Ok(None);
    }
    let by_scope = super::config_ops::live_secret_keys_by_scope(
        lookup,
        &summary.secrets,
        validated.credential_bound,
        None,
    )?;
    let schema_rev = validated.config_schema.schema_revision();
    let matches_digest = expected_digest.is_some_and(|exp| {
        let d_empty = serde_json::to_vec(&serde_json::json!({
            "schema_revision": schema_rev,
            "values": resolved.public_values,
            "secrets": BTreeMap::<String, String>::new(),
        }))
        .map(|b| format!("sha256:{}", hex_lower(&Sha256::digest(&b))));
        if d_empty.is_ok_and(|d| d == exp) {
            return true;
        }
        by_scope.iter().any(|(_, scope_keys)| {
            if validated
                .config_schema
                .resolve_config_write(&materialized, &BTreeMap::new(), scope_keys)
                .is_err()
            {
                return false;
            }
            let keep_map: BTreeMap<String, String> = scope_keys
                .iter()
                .map(|k| (k.clone(), "keep".to_string()))
                .collect();
            serde_json::to_vec(&serde_json::json!({
                "schema_revision": schema_rev,
                "values": resolved.public_values,
                "secrets": keep_map,
            }))
            .map(|b| format!("sha256:{}", hex_lower(&Sha256::digest(&b))))
            .is_ok_and(|d| d == exp)
        })
    });
    if !matches_digest {
        return Ok(None);
    }
    let upgrade_cfg = if existing_cfg
        .as_ref()
        .is_none_or(|c| c.schema_revision != schema_rev)
    {
        Some(PluginUpgradeConfig {
            schema_revision: schema_rev.to_string(),
            values: resolved.public_values.clone(),
            secret_scope_updates: Vec::new(),
        })
    } else {
        None
    };
    let validation_cfg = PluginConfig {
        schema_revision: schema_rev.to_string(),
        revision: existing_cfg.as_ref().map_or(0, |c| c.revision),
        values: resolved.public_values,
    };
    Ok(Some((upgrade_cfg, validation_cfg)))
}

fn prepare_rollforward_target(
    state: &ApiState,
    plugin: &store::plugins::PluginRecord,
    intent: &store::plugins::StagedPluginIntent,
    target_hash: &str,
    actor: UserId,
) -> Result<Option<PreparedRollforward>, PluginServiceError> {
    let Ok((bytes, sig)) = read_recovery_archive(
        state.store.data_dir(),
        intent.artifact_path.as_deref(),
        &intent.target_plugin_id,
        Some(target_hash),
        state,
    ) else {
        return Ok(None);
    };
    let Ok(policy) = build_package_policy_from_store(&state.store, sig.clone()) else {
        return Ok(None);
    };
    let Ok(validated) = validate_cmplugin_bytes(&bytes, &policy) else {
        return Ok(None);
    };
    if validated.package_sha256_hex != target_hash {
        return Ok(None);
    }
    let Some((_, validation_cfg)) = verify_preserve_rollforward_config(
        state.store.as_ref(),
        &plugin.plugin_id,
        &validated,
        intent.target_config_digest.as_deref(),
    )?
    else {
        return Ok(None);
    };
    let (Ok(host), Ok(validation_host)) =
        (state.plugins.wasm_host(), state.plugins.validation_host())
    else {
        return Ok(None);
    };
    if compile_validated_package_components(host, &validated).is_err() {
        return Ok(None);
    }
    if validate_grant_targets(state.store.as_ref(), &intent.grants).is_err() {
        return Ok(None);
    }
    let (target_ver, need_register) = match state
        .store
        .get_plugin_version(&plugin.plugin_id, target_hash)?
    {
        Some(existing) => (existing, false),
        None => {
            let Ok(rel) = materialize_immutable_package_sync(
                state.store.data_dir(),
                &bytes,
                sig.as_deref(),
                &validated,
            ) else {
                return Ok(None);
            };
            (version_metadata_from_validated(&validated, rel), true)
        }
    };
    if validate_installed_components(
        state,
        validation_host,
        &plugin.plugin_id,
        &target_ver,
        actor,
        Some(validation_cfg),
    )
    .is_err()
    {
        return Ok(None);
    }
    Ok(Some(PreparedRollforward {
        validated,
        target_ver,
        need_register,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn try_roll_forward_preserve_upgrade(
    state: &ApiState,
    claim: &mut store::plugins::PluginCommandClaim,
    plugin: &store::plugins::PluginRecord,
    intent: &store::plugins::StagedPluginIntent,
    target_hash: &str,
    actor: UserId,
    now: i64,
    foreground: bool,
    resume_ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<bool, PluginServiceError> {
    let _pin = state.plugins.staging_pins().pin(target_hash);
    let Some(prep) = prepare_rollforward_target(state, plugin, intent, target_hash, actor)? else {
        return Ok(false);
    };
    let saved_claim = claim.clone();
    let commit_res = run_foreground_app_tx(state, foreground, |app| {
        let Some(cur) = app.get_plugin(&plugin.plugin_id)? else {
            return Err(PluginServiceError::not_found(
                "plugin.missing",
                "plugin not found",
            ));
        };
        if !matches!(
            cur.lifecycle,
            PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
        ) {
            return Err(PluginServiceError::Store(PluginError::NotDrained.into()));
        }
        let Some((target_config, _)) = verify_preserve_rollforward_config(
            app,
            &cur.plugin_id,
            &prep.validated,
            intent.target_config_digest.as_deref(),
        )?
        else {
            return Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "rollforward config or secret scope invalidated before commit",
            ));
        };
        validate_grant_targets(app, &intent.grants)?;
        let op_id = claim.operation_id();
        let reg = prep
            .need_register
            .then_some((&mut *claim, &prep.target_ver, now));
        let switched = app.register_and_apply_plugin_package_switch(
            reg,
            &ApplyPluginPackageSwitch {
                plugin_id: cur.plugin_id.clone(),
                expected_revision: cur.revision,
                operation_id: Some(op_id),
                target_package_hash: target_hash.to_string(),
                state_policy: PluginStatePolicy::Preserve,
                target_config,
                target_grants: intent.grants.clone(),
            },
        )?;
        seed_manifest_task_bindings_in_app(app, &switched, &prep.validated.manifest)?;
        let result = serde_json::json!({
            "plugin_id": switched.plugin_id,
            "package_hash": target_hash,
            "recovered": true,
        });
        app.complete_staged_plugin_operation(
            claim,
            PluginOperationState::Succeeded,
            "switched",
            Some(&result),
            None,
            now,
        )?;
        settle_resume_idempotency_in_app(app, op_id, resume_ctx)?;
        Ok((cur.package_hash, switched))
    });
    let (old_hash, switched) = match commit_res {
        Ok(pair) => pair,
        Err(err) => {
            *claim = saved_claim;
            if is_transient_or_admission_err(&err) {
                return Err(err);
            }
            return Ok(false);
        }
    };

    state
        .plugins
        .in_flight()
        .close_dispatch(&switched.plugin_id, switched.generation.saturating_add(1));
    state
        .plugins
        .cache()
        .evict_hash(&switched.plugin_id, &old_hash);
    state.plugins.cache().evict_plugin(&switched.plugin_id);
    let _ = super::registry_sync::sync_registry_snapshot(state, true);
    Ok(true)
}
