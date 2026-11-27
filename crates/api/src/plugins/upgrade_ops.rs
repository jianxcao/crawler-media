use std::collections::{BTreeMap, BTreeSet};

use plugin_package::{
    SecretWriteAction, ValidatedConfigSchema, ValidatedPackage, validate_cmplugin_bytes,
};
use sha2::{Digest, Sha256};
use store::Store;
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginError, PluginGrant, PluginLifecycle,
    PluginOperationState, PluginRecord, PluginSecretChange, PluginSecretUpdate,
    PluginSecurityScope, PluginSubject, PluginUpgradeConfig, StagedPluginIntent,
    StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::load_app_operation_admin_view;
use super::install_ops::manifest_required_capabilities;
use super::lifecycle_ops::{check_management_replay, check_management_replay_after_write};
use super::reauth::{ConsentPayload, now_unix};
use super::service::{PluginService, run_app_read, run_blocking};
use super::staging::{
    build_package_policy_from_store, hex_lower, map_signature_status,
    read_packaged_archive_for_version, read_staged_upload_for_actor_sync,
};
use super::types::{
    ConfigDiffView, ExecuteUpgradeInput, GrantDiffView, OperationAdminView, PluginServiceError,
    PreparationImpactView, PrepareUpgradeInput, ReauthPurpose, RollbackInput, StateDiffView,
    StateResetPreviewView, UpgradePreparationView, UpgradeTargetConfigInput, map_package_err,
};
use super::upgrade_switch::SwitchRequest;
use super::wasm_runner::compile_validated_package_components;
use crate::management::ApiState;

fn validate_upgrade_config_and_digest(
    store: &Store,
    plugin: &PluginRecord,
    credential_bound: bool,
    site_obs: Option<&store::SiteObservation>,
    schema: &ValidatedConfigSchema,
    input: &UpgradeTargetConfigInput,
    summary_secrets: &[store::plugins::PluginSecretMetadata],
) -> Result<(PluginUpgradeConfig, String), PluginServiceError> {
    if !input.schema_revision.is_empty() && input.schema_revision != schema.schema_revision() {
        return Err(PluginServiceError::config_invalid(
            "target_config schema_revision does not match target package schema",
            Vec::new(),
        ));
    }
    let specific_site = if credential_bound {
        site_obs.map(|o| o.site().id)
    } else {
        None
    };
    let resolved = super::config_ops::resolve_config_write_for_scopes(
        store,
        schema,
        &input.values,
        &input.secrets,
        summary_secrets,
        credential_bound,
        specific_site,
    )?
    .map_err(|fields| PluginServiceError::config_invalid("invalid target_config", fields))?;
    super::config_ops::validate_config_object_grants(store, &resolved.public_values)?;

    let mut action_summary = BTreeMap::new();
    let mut secret_updates = Vec::new();
    for u in resolved.secret_updates {
        let label = match &u.change {
            SecretWriteAction::Keep => "keep",
            SecretWriteAction::Replace(_) => "replace",
            SecretWriteAction::Clear => "clear",
        };
        action_summary.insert(u.key.clone(), label.to_string());
        let change = match u.change {
            SecretWriteAction::Keep => PluginSecretChange::Keep,
            SecretWriteAction::Replace(s) => PluginSecretChange::Replace(s.into_bytes()),
            SecretWriteAction::Clear => PluginSecretChange::Clear,
        };
        secret_updates.push(PluginSecretUpdate { key: u.key, change });
    }
    let digest_payload = serde_json::json!({
        "schema_revision": schema.schema_revision(),
        "values": resolved.public_values,
        "secrets": action_summary,
    });
    let canonical =
        serde_json::to_vec(&digest_payload).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let digest = format!("sha256:{}", hex_lower(&Sha256::digest(&canonical)));

    let bind_site_implicit =
        credential_bound && !schema.secret_fields().is_empty() && site_obs.is_some();
    let secret_scope_updates = if secret_updates.is_empty() && !bind_site_implicit {
        Vec::new()
    } else if !matches!(
        plugin.lifecycle,
        PluginLifecycle::Enabled | PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
    ) {
        return Err(PluginServiceError::conflict(
            "plugin.not_drained",
            "plugin is mid-transition",
        ));
    } else if !credential_bound {
        vec![(PluginSecurityScope::global(), secret_updates)]
    } else if let Some(obs) = site_obs {
        vec![(PluginSecurityScope::for_site(obs), secret_updates)]
    } else {
        let mut distinct: Vec<PluginSecurityScope> = Vec::new();
        for s in summary_secrets {
            if s.configured
                && super::config_ops::is_live_secret_scope(store, true, None, &s.scope)?
                && let Some(sid) = s.scope.site_id()
                && !distinct.iter().any(|d| d.site_id() == Some(sid))
            {
                distinct.push(s.scope.clone());
            }
        }
        if distinct.len() == 1 {
            vec![(distinct[0].clone(), secret_updates)]
        } else {
            return Err(PluginServiceError::invalid(
                "plugin.scope_invalid",
                "credential-bound plugin requires site_id or a single existing site secret scope",
            ));
        }
    };

    let upgrade_cfg = PluginUpgradeConfig {
        schema_revision: schema.schema_revision().to_string(),
        values: resolved.public_values,
        secret_scope_updates,
    };
    Ok((upgrade_cfg, digest))
}

impl PluginService {
    pub async fn prepare_upgrade(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: PrepareUpgradeInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "upgrade-prepare",
        )
        .await?
        {
            return Ok(replayed);
        }
        super::service::require_foreground_write_admitted(state)?;
        validate_prepare_upgrade_input(&input)?;
        let ctx = match load_prepare_upgrade_context(state, plugin_id, &input).await {
            Ok(c) => c,
            Err(err) => {
                if matches!(
                    err,
                    PluginServiceError::Conflict { .. }
                        | PluginServiceError::Store(store::StoreError::Plugin(
                            PluginError::OperationActive(_) | PluginError::Conflict
                        ))
                ) && let Some(replayed) = check_management_replay_after_write(
                    state,
                    input.actor_user_id,
                    Some(plugin_id),
                    &input.idempotency_key,
                    &input.request,
                    "upgrade-prepare",
                )
                .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };
        let validated = compile_upgrade_source(
            self,
            state,
            plugin_id,
            input.actor_user_id,
            &input.version,
            input.upload_id,
            ctx.historical_ver,
        )
        .await?;
        let analysis = analyze_upgrade_target(
            state,
            plugin_id,
            &ctx.plugin,
            &ctx.config_summary,
            &validated,
            &input,
        )
        .await?;
        let (prep_view, pkg_hash, ver_str) = build_upgrade_prep_view(
            self,
            plugin_id,
            &input,
            &ctx.plugin,
            &ctx.current_version,
            &ctx.existing_grants,
            &ctx.config_summary,
            &validated,
            analysis,
        )?;
        persist_prepare_upgrade_op(state, plugin_id, &input, pkg_hash, ver_str, &prep_view).await
    }

    pub async fn execute_upgrade(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: ExecuteUpgradeInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        self.run_package_switch(
            state,
            plugin_id,
            SwitchRequest {
                actor_user_id: input.actor_user_id,
                op_kind: StagedPluginOpKind::Upgrade,
                expected_package_hash: None,
                preparation_id: input.preparation_id,
                consent_token: input.consent_token,
                grants: input.grants,
                state_policy: input.state_policy,
                target_config_digest: input.target_config_digest,
                reauth_token: input.reauth_token,
                reauth_purpose: ReauthPurpose::PluginUpgrade,
                expected_revision: input.expected_revision,
                idempotency_key: input.idempotency_key,
                request: input.request,
            },
        )
        .await
    }

    pub async fn rollback_plugin(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: RollbackInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        self.run_package_switch(
            state,
            plugin_id,
            SwitchRequest {
                actor_user_id: input.actor_user_id,
                op_kind: StagedPluginOpKind::Rollback,
                expected_package_hash: Some(input.package_hash),
                preparation_id: input.preparation_id,
                consent_token: input.consent_token,
                grants: input.grants,
                state_policy: input.state_policy,
                target_config_digest: input.target_config_digest,
                reauth_token: input.reauth_token,
                reauth_purpose: ReauthPurpose::PluginRollback,
                expected_revision: input.expected_revision,
                idempotency_key: input.idempotency_key,
                request: input.request,
            },
        )
        .await
    }
}

struct UpgradeContext {
    plugin: PluginRecord,
    current_version: store::plugins::PluginVersionMetadata,
    existing_grants: Vec<PluginGrant>,
    config_summary: store::plugins::PluginConfigSummary,
    historical_ver: Option<store::plugins::PluginVersionMetadata>,
}

struct UpgradeAnalysis {
    upgrade_cfg: PluginUpgradeConfig,
    target_config_digest: String,
    reset_preview: Option<StateResetPreviewView>,
    existing_valid: bool,
    val_errs: Vec<plugin_package::SchemaFieldError>,
}

fn validate_prepare_upgrade_input(input: &PrepareUpgradeInput) -> Result<(), PluginServiceError> {
    if input.repo_id.is_some() {
        return Err(PluginServiceError::Unprocessable {
            code: "plugin.unsupported",
            message: "remote plugin repositories are deferred to Phase 2".into(),
        });
    }
    if !matches!(input.state_policy.as_str(), "preserve" | "reset") {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "state_policy must be `preserve` or `reset`",
        ));
    }
    if input.upload_id.is_some() && input.package_hash.is_some() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "specify at most one of upload_id or package_hash",
        ));
    }
    if input.upload_id.is_none() && input.package_hash.is_none() && input.version.trim().is_empty()
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "specify upload_id, package_hash, or target version",
        ));
    }
    Ok(())
}

async fn load_prepare_upgrade_context(
    state: &ApiState,
    plugin_id: &str,
    input: &PrepareUpgradeInput,
) -> Result<UpgradeContext, PluginServiceError> {
    let pid_read = plugin_id.to_string();
    let upload_id = input.upload_id;
    let package_hash = input.package_hash.clone();
    let target_version = input.version.trim().to_string();
    let expected_revision = input.expected_revision;
    run_app_read(state, move |store| {
        let plugin = store
            .get_plugin(&pid_read)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        if plugin.revision != expected_revision {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "plugin revision mismatch",
            ));
        }
        if let Some(active) = plugin.active_operation_id {
            return Err(PluginServiceError::Store(
                PluginError::OperationActive(active).into(),
            ));
        }
        let current_version = store
            .get_plugin_version(&pid_read, &plugin.package_hash)?
            .ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "active version missing")
            })?;
        let existing_grants = store.get_plugin_grants(&pid_read)?;
        let config_summary = store.get_plugin_config_summary(&pid_read)?;
        let historical_ver = if let Some(hash) = &package_hash {
            Some(store.get_plugin_version(&pid_read, hash)?.ok_or_else(|| {
                PluginServiceError::not_found(
                    "plugin.version_missing",
                    "historical package version not found",
                )
            })?)
        } else if upload_id.is_none() {
            Some(
                store
                    .get_plugin_version_by_semver(&pid_read, &target_version)?
                    .ok_or_else(|| {
                        PluginServiceError::not_found(
                            "plugin.version_missing",
                            "historical package version not found",
                        )
                    })?,
            )
        } else {
            None
        };
        Ok(UpgradeContext {
            plugin,
            current_version,
            existing_grants,
            config_summary,
            historical_ver,
        })
    })
    .await
}

async fn compile_upgrade_source(
    service: &PluginService,
    state: &ApiState,
    plugin_id: &str,
    actor_user_id: domain::UserId,
    expected_version: &str,
    upload_id: Option<Uuid>,
    historical_ver: Option<store::plugins::PluginVersionMetadata>,
) -> Result<ValidatedPackage, PluginServiceError> {
    let s = state.clone();
    let host = service
        .wasm_host()
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
        .clone();
    let expected_pid = plugin_id.to_string();
    let expected_ver = expected_version.to_string();
    run_blocking(move || {
        let (pkg_bytes, sig_text) = if let Some(uid) = upload_id {
            let (b, sig, _) =
                read_staged_upload_for_actor_sync(s.store.data_dir(), uid, Some(actor_user_id))?;
            (b, sig)
        } else if let Some(hist) = historical_ver {
            read_packaged_archive_for_version(s.store.data_dir(), &hist)?
        } else {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "missing upgrade source",
            ));
        };
        let policy = build_package_policy_from_store(&s.store, sig_text)?;
        let pkg = validate_cmplugin_bytes(&pkg_bytes, &policy).map_err(map_package_err)?;
        if pkg.manifest.id != expected_pid
            || (!expected_ver.is_empty() && pkg.manifest.version != expected_ver)
        {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_package",
                "package manifest id/version mismatch",
            ));
        }
        compile_validated_package_components(&host, &pkg)
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;
        Ok(pkg)
    })
    .await
}

fn evaluate_existing_config_for_upgrade(
    store: &Store,
    new_schema: &ValidatedConfigSchema,
    no_saved_config: bool,
    existing_values: &serde_json::Value,
    summary_secrets: &[store::plugins::PluginSecretMetadata],
    credential_bound: bool,
    specific_site: Option<domain::SiteId>,
    state_policy: &str,
) -> Result<(bool, Vec<plugin_package::SchemaFieldError>), PluginServiceError> {
    let materialized = match if no_saved_config {
        new_schema.validate_values(existing_values, true)
    } else {
        Ok(existing_values.clone())
    } {
        Ok(m) => m,
        Err(errs) => return Ok((false, errs)),
    };
    match super::config_ops::resolve_config_write_for_scopes(
        store,
        new_schema,
        &materialized,
        &BTreeMap::new(),
        summary_secrets,
        credential_bound,
        specific_site,
    )? {
        Ok(resolved_existing) => {
            if state_policy == "preserve" {
                super::config_ops::validate_config_object_grants(
                    store,
                    &resolved_existing.public_values,
                )?;
            }
            let ok = super::config_ops::validate_config_object_grants(
                store,
                &resolved_existing.public_values,
            )
            .is_ok();
            Ok((ok, Vec::new()))
        }
        Err(errs) => Ok((false, errs)),
    }
}

async fn analyze_upgrade_target(
    state: &ApiState,
    plugin_id: &str,
    plugin: &PluginRecord,
    config_summary: &store::plugins::PluginConfigSummary,
    validated: &ValidatedPackage,
    input: &PrepareUpgradeInput,
) -> Result<UpgradeAnalysis, PluginServiceError> {
    let summary_secrets = config_summary.secrets.clone();
    let no_saved_config = config_summary.config.is_none();
    let existing_values = config_summary
        .config
        .as_ref()
        .map(|c| c.values.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    let pid_check = plugin_id.to_string();
    let hash_check = validated.package_sha256_hex.clone();
    let ver_check = validated.manifest.version.clone();
    let reset_scopes_clone = input.reset_scopes.clone();
    let state_policy_str = input.state_policy.clone();
    let target_cfg_input = input.target_config.clone();
    let new_schema = validated.config_schema.clone();
    let cred_bound = validated.credential_bound;
    let site_id = input.site_id;
    let plugin_for_read = plugin.clone();
    let ui_remote_name = validated
        .manifest
        .ui
        .as_ref()
        .map(|u| u.remote_name.clone());

    run_app_read(state, move |store| {
        if let Some(existing_ver) = store.get_plugin_version_by_semver(&pid_check, &ver_check)?
            && existing_ver.package_hash != hash_check
        {
            return Err(PluginServiceError::Store(
                PluginError::PackageConflict.into(),
            ));
        }
        if let Some(remote_name) = ui_remote_name.as_deref()
            && let Some(owner) =
                store.find_conflicting_remote_name_plugin(&pid_check, remote_name)?
        {
            return Err(PluginServiceError::conflict(
                "plugin.remote_name_conflict",
                format!("ui.remote_name `{remote_name}` is already registered by plugin `{owner}`"),
            ));
        }
        let site_obs = match site_id {
            Some(sid) => Some(store.observe_site(sid)?.ok_or_else(|| {
                PluginServiceError::invalid(
                    "plugin.scope_invalid",
                    "referenced site_id does not exist",
                )
            })?),
            None => None,
        };
        let specific_site = if cred_bound {
            site_obs.as_ref().map(|o| o.site().id)
        } else {
            None
        };
        let (upgrade_cfg, target_config_digest) = validate_upgrade_config_and_digest(
            store,
            &plugin_for_read,
            cred_bound,
            site_obs.as_ref(),
            &new_schema,
            &target_cfg_input,
            &summary_secrets,
        )?;
        let (existing_valid, val_errs) = evaluate_existing_config_for_upgrade(
            store,
            &new_schema,
            no_saved_config,
            &existing_values,
            &summary_secrets,
            cred_bound,
            specific_site,
            &state_policy_str,
        )?;
        let reset_preview = if state_policy_str == "reset" {
            let p = store.preview_plugin_state_reset(&pid_check, reset_scopes_clone.as_deref())?;
            Some(StateResetPreviewView {
                kv_entries: p.kv_entries,
                blob_entries: p.blob_entries,
                kv_bytes: p.kv_bytes,
                blob_bytes: p.blob_bytes,
                state_revision: p.state_revision,
            })
        } else {
            None
        };
        Ok(UpgradeAnalysis {
            upgrade_cfg,
            target_config_digest,
            reset_preview,
            existing_valid,
            val_errs,
        })
    })
    .await
}

#[allow(clippy::too_many_arguments)]
fn build_upgrade_prep_view(
    service: &PluginService,
    plugin_id: &str,
    input: &PrepareUpgradeInput,
    plugin: &PluginRecord,
    current_version: &store::plugins::PluginVersionMetadata,
    existing_grants: &[PluginGrant],
    config_summary: &store::plugins::PluginConfigSummary,
    validated: &ValidatedPackage,
    analysis: UpgradeAnalysis,
) -> Result<(UpgradePreparationView, String, String), PluginServiceError> {
    let old_state_schema: u32 = current_version.state_schema.parse().unwrap_or(0);
    let new_state_schema = validated.state_schema;
    let schema_changed = old_state_schema != new_state_schema
        || current_version.credential_bound != validated.credential_bound;
    let preserve_allowed = !schema_changed && analysis.existing_valid;
    if input.state_policy == "preserve" && schema_changed {
        return Err(PluginServiceError::Store(
            PluginError::StateSchemaMismatch.into(),
        ));
    }
    if input.state_policy == "preserve" && !analysis.existing_valid {
        return Err(PluginServiceError::config_invalid(
            "existing configuration does not satisfy new schema; supply valid target_config",
            analysis.val_errs,
        ));
    }

    let required_caps = manifest_required_capabilities(&validated.manifest);
    let req_set: BTreeSet<&str> = required_caps.iter().map(String::as_str).collect();
    let existing_caps: BTreeSet<&str> = existing_grants
        .iter()
        .map(|g| g.capability.as_str())
        .collect();
    let added: Vec<PluginGrant> = required_caps
        .iter()
        .filter(|cap| !existing_caps.contains(cap.as_str()))
        .map(|cap| PluginGrant {
            capability: cap.clone(),
            scope: store::plugins::PluginGrantScope::Global,
            sensitive: false,
        })
        .collect();
    let kept: Vec<PluginGrant> = existing_grants
        .iter()
        .filter(|g| req_set.contains(g.capability.as_str()))
        .cloned()
        .collect();
    let removed: Vec<PluginGrant> = existing_grants
        .iter()
        .filter(|g| !req_set.contains(g.capability.as_str()))
        .cloned()
        .collect();
    let pkg_hash = validated.package_sha256_hex.clone();
    let ver_str = validated.manifest.version.clone();
    let (sig_status, signing_key_id) = map_signature_status(&validated.signature.status);
    let preparation_id = Uuid::new_v4();
    let (consent_token, expires_at) = service.consent().mint(
        input.actor_user_id,
        preparation_id,
        ConsentPayload::Upgrade {
            plugin_id: plugin_id.to_string(),
            version: ver_str.clone(),
            package_hash: pkg_hash.clone(),
            upload_id: input.upload_id,
            base_revision: input.expected_revision,
            pre_drained_to_revision: None,
            state_policy: input.state_policy.clone(),
            target_config_digest: analysis.target_config_digest.clone(),
            target_config: Some(analysis.upgrade_cfg),
            reset_scopes: input.reset_scopes.clone(),
            allowed_capabilities: required_caps.clone(),
        },
    );
    let view = UpgradePreparationView {
        preparation_id,
        plugin_id: plugin_id.to_string(),
        version: ver_str.clone(),
        package_hash: pkg_hash.clone(),
        base_revision: input.expected_revision,
        signature_status: sig_status.to_string(),
        signing_key_id,
        state_policy: input.state_policy.clone(),
        target_config_digest: analysis.target_config_digest.clone(),
        grant_diff: GrantDiffView {
            added,
            kept,
            removed,
            required: required_caps,
        },
        config_diff: ConfigDiffView {
            old_schema_revision: config_summary
                .config
                .as_ref()
                .map(|c| c.schema_revision.clone()),
            new_schema_revision: validated.config_schema.schema_revision().to_string(),
            target_config_digest: analysis.target_config_digest,
            existing_valid: analysis.existing_valid,
            validation_errors: analysis.val_errs,
        },
        state_diff: StateDiffView {
            old_state_schema,
            new_state_schema,
            schema_changed,
            preserve_allowed,
            reset_preview: analysis.reset_preview,
        },
        impact: PreparationImpactView {
            summary: format!("Upgrade {plugin_id} to {ver_str}"),
            requires_drain: plugin.lifecycle == PluginLifecycle::Enabled,
            requires_reauth: input.state_policy == "reset",
            resets_state: input.state_policy == "reset",
        },
        consent_token,
        expires_at,
    };
    Ok((view, pkg_hash, ver_str))
}

async fn persist_prepare_upgrade_op(
    state: &ApiState,
    plugin_id: &str,
    input: &PrepareUpgradeInput,
    pkg_hash: String,
    ver_str: String,
    prep_view: &UpgradePreparationView,
) -> Result<OperationAdminView, PluginServiceError> {
    let result_json =
        serde_json::to_value(prep_view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let pid = plugin_id.to_string();
    let actor = input.actor_user_id;
    let expected_rev = input.expected_revision;
    let idem = input.idempotency_key.clone();
    let req = input.request.clone();
    let state_pol = input.state_policy.clone();
    let digest = prep_view.config_diff.target_config_digest.clone();
    run_app_write_tx(state, move |app| {
        let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: idem,
            request: req,
            op_kind: StagedPluginOpKind::UpgradePrepare,
            expected_revision: expected_rev,
            intent: StagedPluginIntent {
                target_plugin_id: pid.clone(),
                package_hash: Some(pkg_hash),
                version: Some(ver_str),
                artifact_path: None,
                state_policy: Some(state_pol),
                target_config_digest: Some(digest),
                grants: Vec::new(),
            },
            cause: PluginCommandCause::root(format!("upgrade-prepare:{pid}")),
        })?;
        if accepted.replayed {
            return load_app_operation_admin_view(app, accepted.operation.operation_id);
        }
        let claim = app.claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            now_unix(),
        )?;
        app.complete_staged_plugin_operation(
            &claim,
            PluginOperationState::Succeeded,
            "prepared",
            Some(&result_json),
            None,
            now_unix(),
        )?;
        load_app_operation_admin_view(app, accepted.operation.operation_id)
    })
    .await
}
