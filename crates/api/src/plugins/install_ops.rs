use std::collections::BTreeSet;

use plugin_package::{PluginManifest, validate_cmplugin_bytes};
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginGrant, PluginGrantScope,
    PluginOperationState, PluginRecord, PluginSubject, PluginTaskDefinition, StagedPluginIntent,
    StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    OpBegin, check_install_replay, check_install_replay_after_write, is_consent_stale_err,
    load_app_operation_admin_view,
};
use super::reauth::{ConsentPayload, now_unix};
use super::service::{PluginService, run_app_read, run_blocking};
use super::staging::{
    build_package_policy_from_store, gc_unreferenced_artifacts_with_store, map_signature_status,
    materialize_immutable_package_sync, read_staged_upload_for_actor_sync, signature_fingerprint,
    version_metadata_from_validated,
};
use super::types::{
    ExecuteInstallInput, GrantDiffView, InstallationPreparationView, OperationAdminView,
    PluginServiceError, PreparationImpactView, PrepareInstallInput, map_package_err,
};
use super::wasm_runner::compile_validated_package_components;
use crate::management::ApiState;

pub(crate) fn manifest_required_capabilities(manifest: &PluginManifest) -> Vec<String> {
    let mut caps = BTreeSet::new();
    for comp in &manifest.components {
        for p in &comp.permissions {
            caps.insert(p.clone());
        }
    }
    if let Some(ui) = &manifest.ui {
        for c in &ui.capabilities {
            caps.insert(c.clone());
        }
    }
    caps.into_iter().collect()
}

pub(crate) fn validate_grants_within_manifest(
    grants: &[PluginGrant],
    allowed_caps: &[String],
) -> Result<(), PluginServiceError> {
    let allowed: BTreeSet<&str> = allowed_caps.iter().map(String::as_str).collect();
    for g in grants {
        if !allowed.contains(g.capability.as_str()) {
            return Err(PluginServiceError::invalid(
                "plugin.scope_invalid",
                format!("capability `{}` is not declared in manifest", g.capability),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_grant_targets<L: super::config_ops::ConfigObjectLookup>(
    lookup: &L,
    grants: &[PluginGrant],
) -> Result<(), PluginServiceError> {
    for g in grants {
        match &g.scope {
            PluginGrantScope::Site(sid) => {
                if !lookup.site_exists(*sid)? {
                    return Err(PluginServiceError::invalid(
                        "plugin.scope_invalid",
                        format!("granted site `{sid}` does not exist"),
                    ));
                }
            }
            PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
                let sid = super::ports::parse_site_scope_id(id).ok_or_else(|| {
                    PluginServiceError::invalid(
                        "plugin.scope_invalid",
                        format!("invalid site grant scope id `{id}`"),
                    )
                })?;
                if !lookup.site_exists(sid)? {
                    return Err(PluginServiceError::invalid(
                        "plugin.scope_invalid",
                        format!("granted site `{sid}` does not exist"),
                    ));
                }
            }
            PluginGrantScope::Object { kind, id } if kind.trim() == "downloader" => {
                let did = super::ports::parse_downloader_scope_id(id).ok_or_else(|| {
                    PluginServiceError::invalid(
                        "plugin.scope_invalid",
                        format!("invalid downloader grant scope id `{id}`"),
                    )
                })?;
                if !lookup.downloader_exists(did)? {
                    return Err(PluginServiceError::invalid(
                        "plugin.scope_invalid",
                        format!("granted downloader `{did}` does not exist"),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn seed_manifest_task_bindings_in_app(
    app: &store::App<'_>,
    plugin: &PluginRecord,
    manifest: &PluginManifest,
) -> Result<(), PluginServiceError> {
    for comp in &manifest.components {
        for task in &comp.tasks {
            if app
                .get_plugin_task_binding(&plugin.plugin_id, &task.id)?
                .is_none()
            {
                let secs = task.schedule.seconds.clamp(60, 86_400);
                app.save_plugin_task_binding(
                    plugin,
                    &task.id,
                    0,
                    &PluginTaskDefinition {
                        enabled: false,
                        interval_secs: Some(secs),
                        public_config: serde_json::json!({}),
                    },
                )?;
            }
        }
    }
    Ok(())
}

impl PluginService {
    pub async fn prepare_install(
        &self,
        state: &ApiState,
        input: PrepareInstallInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_install_replay(
            state,
            input.actor_user_id,
            &input.idempotency_key,
            &input.request,
            "install-prepare",
        )
        .await?
        {
            return Ok(replayed);
        }
        super::service::require_foreground_write_admitted(state)?;

        if input.repo_id.is_some() {
            return Err(PluginServiceError::Unprocessable {
                code: "plugin.unsupported",
                message: "remote plugin repositories are deferred to Phase 2".into(),
            });
        }
        let upload_id = input.upload_id.ok_or_else(|| {
            PluginServiceError::invalid("plugin.invalid_input", "upload_id is required")
        })?;

        let s = state.clone();
        let actor_for_upload = input.actor_user_id;
        let expected_pid = input.plugin_id.clone();
        let expected_ver = input.version.clone();
        let host = self
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        let validated = run_blocking(move || {
            let (pkg_bytes, sig_text, _) = read_staged_upload_for_actor_sync(
                s.store.data_dir(),
                upload_id,
                Some(actor_for_upload),
            )?;
            let policy = build_package_policy_from_store(&s.store, sig_text)?;
            let pkg = validate_cmplugin_bytes(&pkg_bytes, &policy).map_err(map_package_err)?;
            if expected_pid
                .as_deref()
                .map(str::trim)
                .is_some_and(|id| !id.is_empty() && pkg.manifest.id != id)
                || expected_ver
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|v| !v.is_empty() && pkg.manifest.version != v)
            {
                return Err(PluginServiceError::invalid(
                    "plugin.invalid_package",
                    "package manifest id/version mismatch",
                ));
            }
            compile_validated_package_components(&host, &pkg).map_err(|e| {
                PluginServiceError::invalid("plugin.invalid_component", e.to_string())
            })?;
            Ok(pkg)
        })
        .await?;

        let prep_view = build_install_prep_view(self, input.actor_user_id, upload_id, &validated);
        let result_json =
            serde_json::to_value(&prep_view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        let plugin_id = prep_view.plugin_id;
        let pkg_hash = prep_view.package_hash;
        let version_str = prep_view.version;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key;
        let req = input.request;
        run_app_write_tx(state, move |app| {
            let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(actor),
                idempotency_key: idem,
                request: req,
                op_kind: StagedPluginOpKind::InstallPrepare,
                expected_revision: 0,
                intent: StagedPluginIntent {
                    target_plugin_id: plugin_id.clone(),
                    package_hash: Some(pkg_hash),
                    version: Some(version_str),
                    artifact_path: None,
                    state_policy: None,
                    target_config_digest: None,
                    grants: Vec::new(),
                },
                cause: PluginCommandCause::root(format!("install-prepare:{plugin_id}")),
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

    pub async fn execute_install(
        &self,
        state: &ApiState,
        input: ExecuteInstallInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_install_replay(
            state,
            input.actor_user_id,
            &input.idempotency_key,
            &input.request,
            "install",
        )
        .await?
        {
            return Ok(replayed);
        }
        super::service::require_foreground_write_admitted(state)?;
        if input.expected_revision != 0 {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "install requires expected_revision 0",
            ));
        }

        let g_check = input.grants.clone();
        run_app_read(state, move |store| validate_grant_targets(store, &g_check)).await?;

        let payload = match peek_install_consent(self, state, &input).await? {
            Ok(p) => p,
            Err(replayed) => return Ok(replayed),
        };
        let ConsentPayload::Install {
            plugin_id,
            version,
            package_hash,
            upload_id,
            ..
        } = payload
        else {
            return Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "consent_token is not for installation",
            ));
        };

        let pin = self.staging_pins().pin(&package_hash);
        let (mut claim, _active_op_guard) = match begin_and_claim_install(
            self,
            state,
            &input,
            &plugin_id,
            &version,
            &package_hash,
            upload_id,
        )
        .await
        {
            Ok(OpBegin::Replayed(view)) => return Ok(*view),
            Ok(OpBegin::Proceed(pair)) => pair,
            Err(err) => {
                drop(pin);
                if (is_consent_stale_err(&err)
                    || matches!(
                        err,
                        PluginServiceError::Conflict { .. } | PluginServiceError::Store(_)
                    ))
                    && let Some(replayed) = check_install_replay_after_write(
                        state,
                        input.actor_user_id,
                        &input.idempotency_key,
                        &input.request,
                        "install",
                    )
                    .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        let s = state.clone();
        let host = self
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        let grants_exec = input.grants;
        let actor_exec = input.actor_user_id;
        let expected_hash = package_hash.clone();
        let op_id = claim.operation_id();

        run_blocking(move || {
            let _guard = _active_op_guard;
            let res = execute_staged_install_sync(
                &s,
                &host,
                &mut claim,
                Some(actor_exec),
                upload_id,
                &expected_hash,
                &grants_exec,
            );
            drop(pin);
            if res
                .as_ref()
                .is_err_and(|e| !super::recovery::is_transient_or_admission_err(e))
            {
                let _ = super::op_settlement::settle_failed_staged_operation_mode(
                    &s,
                    op_id,
                    "install-failed",
                    true,
                );
                let _ = gc_unreferenced_artifacts_with_store(&s.store, s.plugins.staging_pins());
            }
            res
        })
        .await
    }
}

async fn peek_install_consent(
    service: &PluginService,
    state: &ApiState,
    input: &ExecuteInstallInput,
) -> Result<Result<ConsentPayload, OperationAdminView>, PluginServiceError> {
    let grants = input.grants.clone();
    match service.consent().peek(
        input.actor_user_id,
        Some(input.preparation_id),
        &input.consent_token,
        |p| match p {
            ConsentPayload::Install {
                allowed_capabilities,
                ..
            } => {
                validate_grants_within_manifest(&grants, allowed_capabilities)?;
                Ok(p.clone())
            }
            _ => Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "consent_token is not for installation",
            )),
        },
    ) {
        Ok(p) => Ok(Ok(p)),
        Err(err) => {
            if is_consent_stale_err(&err)
                && let Some(replayed) = check_install_replay_after_write(
                    state,
                    input.actor_user_id,
                    &input.idempotency_key,
                    &input.request,
                    "install",
                )
                .await?
            {
                return Ok(Err(replayed));
            }
            Err(err)
        }
    }
}

fn build_install_prep_view(
    service: &PluginService,
    actor_user_id: domain::UserId,
    upload_id: Uuid,
    validated: &plugin_package::ValidatedPackage,
) -> InstallationPreparationView {
    let plugin_id = validated.manifest.id.clone();
    let pkg_hash = validated.package_sha256_hex.clone();
    let version_str = validated.manifest.version.clone();
    let required_caps = manifest_required_capabilities(&validated.manifest);
    let suggested_grants: Vec<PluginGrant> = required_caps
        .iter()
        .map(|cap| PluginGrant {
            capability: cap.clone(),
            scope: PluginGrantScope::Global,
            sensitive: false,
        })
        .collect();
    let (sig_status, signing_key_id) = map_signature_status(&validated.signature.status);
    let fingerprint = signature_fingerprint(&validated.signature.status);
    let preparation_id = Uuid::new_v4();
    let (consent_token, expires_at) = service.consent().mint(
        actor_user_id,
        preparation_id,
        ConsentPayload::Install {
            plugin_id: plugin_id.clone(),
            version: version_str.clone(),
            package_hash: pkg_hash.clone(),
            upload_id,
            allowed_capabilities: required_caps.clone(),
        },
    );
    InstallationPreparationView {
        preparation_id,
        plugin_id: plugin_id.clone(),
        version: version_str.clone(),
        package_hash: pkg_hash,
        base_revision: 0,
        signature_status: sig_status.to_string(),
        signing_key_id,
        fingerprint,
        required_capabilities: required_caps.clone(),
        grant_diff: GrantDiffView {
            added: suggested_grants,
            kept: Vec::new(),
            removed: Vec::new(),
            required: required_caps,
        },
        config_schema: validated.config_schema.root_json().clone(),
        config_schema_revision: validated.config_schema.schema_revision().to_string(),
        state_schema: validated.state_schema,
        impact: PreparationImpactView {
            summary: format!("Install {plugin_id} {version_str}"),
            requires_drain: false,
            requires_reauth: false,
            resets_state: false,
        },
        consent_token,
        expires_at,
    }
}

async fn begin_and_claim_install(
    service: &PluginService,
    state: &ApiState,
    input: &ExecuteInstallInput,
    plugin_id: &str,
    version: &str,
    package_hash: &str,
    upload_id: Uuid,
) -> Result<
    OpBegin<(
        store::plugins::PluginCommandClaim,
        Option<super::service::ActiveStagedOpGuard>,
    )>,
    PluginServiceError,
> {
    let pid = plugin_id.to_string();
    let pkg_hash = package_hash.to_string();
    let ver = version.to_string();
    let actor = input.actor_user_id;
    let prep_id = input.preparation_id;
    let consent_tok = input.consent_token.clone();
    let consent = service.consent().clone();
    let idem = input.idempotency_key.clone();
    let req = input.request.clone();
    let grants_for_op = input.grants.clone();
    let grants_for_check = input.grants.clone();
    let st_guard = state.clone();
    run_app_write_tx(state, move |app| {
        let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: idem,
            request: req,
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: pid.clone(),
                package_hash: Some(pkg_hash.clone()),
                version: Some(ver),
                artifact_path: Some(format!("plugins/uploads/{upload_id}/package.cmplugin")),
                state_policy: None,
                target_config_digest: None,
                grants: grants_for_op,
            },
            cause: PluginCommandCause::root(format!("install:{pid}")),
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
        let _ = consent.peek_consume(actor, Some(prep_id), &consent_tok, |p| match p {
            ConsentPayload::Install {
                plugin_id: cp,
                package_hash: ch,
                allowed_capabilities,
                ..
            } if cp == &pid && ch == &pkg_hash => {
                validate_grants_within_manifest(&grants_for_check, allowed_capabilities)
            }
            _ => Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "consent_token is not for installation",
            )),
        })?;
        let guard = st_guard.plugins.try_acquire_staged_op(claim.operation_id());
        Ok(OpBegin::Proceed((claim, guard)))
    })
    .await
}

fn execute_staged_install_sync(
    s: &ApiState,
    host: &plugin_host::PluginHost,
    claim: &mut store::plugins::PluginCommandClaim,
    expected_actor: Option<domain::UserId>,
    upload_id: Uuid,
    expected_hash: &str,
    grants_exec: &[PluginGrant],
) -> Result<OperationAdminView, PluginServiceError> {
    let op_id = claim.operation_id();
    let (pkg_bytes, sig_text, meta) =
        read_staged_upload_for_actor_sync(s.store.data_dir(), upload_id, expected_actor)?;
    if meta.package_hash != expected_hash {
        return Err(PluginServiceError::conflict(
            "plugin.package_conflict",
            "staged upload hash mismatch",
        ));
    }
    let policy = build_package_policy_from_store(&s.store, sig_text.clone())?;
    let validated = validate_cmplugin_bytes(&pkg_bytes, &policy).map_err(map_package_err)?;
    if validated.package_sha256_hex != expected_hash {
        return Err(PluginServiceError::conflict(
            "plugin.package_conflict",
            "package sha256 mismatch",
        ));
    }
    compile_validated_package_components(host, &validated)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?;
    let saved_phase_claim = claim.clone();
    if let Err(err) = super::service::run_foreground_app_tx(s, true, |app| {
        Ok(app.advance_staged_plugin_phase(claim, "staging", None, now_unix())?)
    }) {
        *claim = saved_phase_claim;
        return Err(err);
    }
    let artifact_rel = materialize_immutable_package_sync(
        s.store.data_dir(),
        &pkg_bytes,
        sig_text.as_deref(),
        &validated,
    )?;
    let version_meta = version_metadata_from_validated(&validated, artifact_rel);
    let saved_claim = claim.clone();
    let committed = super::service::run_foreground_app_tx(s, true, |app| {
        validate_grant_targets(app, grants_exec)?;
        let now = now_unix();
        let record = app.register_staged_plugin_version(claim, &version_meta, grants_exec, now)?;
        seed_manifest_task_bindings_in_app(app, &record, &validated.manifest)?;
        let result = serde_json::json!({
            "plugin_id": record.plugin_id,
            "package_hash": record.package_hash,
            "version": version_meta.version,
            "lifecycle": record.lifecycle,
        });
        app.complete_staged_plugin_operation(
            claim,
            PluginOperationState::Succeeded,
            "installed",
            Some(&result),
            None,
            now,
        )?;
        let view = load_app_operation_admin_view(app, op_id)?;
        Ok((view, record.plugin_id, record.generation))
    });
    match committed {
        Ok((view, pid, installed_gen)) => {
            s.plugins
                .in_flight()
                .close_dispatch(&pid, installed_gen.saturating_add(1));
            Ok(view)
        }
        Err(err) => {
            *claim = saved_claim;
            Err(err)
        }
    }
}
