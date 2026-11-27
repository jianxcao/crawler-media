use std::sync::Arc;

use domain::UserId;
use plugin_package::{ArchiveLimits, PackageError, validate_cmplugin_bytes};
use sha2::{Digest, Sha256};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginGrant,
    PluginGrantScope, PluginLifecycle, PluginOperationState, PluginRecord, PluginSubject,
    PluginVersionMetadata, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::install_ops::{manifest_required_capabilities, seed_manifest_task_bindings_in_app};
use super::lifecycle_ops::sync_all_plugin_tasks;
use super::reauth::now_unix;
use super::service::{
    PluginService, require_foreground_write_admitted, run_app_read, run_blocking,
};
use super::staging::{
    StagingPinGuard, build_package_policy_from_store, gc_unreferenced_artifacts_with_store,
    materialize_immutable_package_sync, version_metadata_from_validated,
};
use super::types::{DisablePluginInput, PluginDetailView, PluginServiceError, map_package_err};
use super::wasm_runner::compile_validated_package_components;
use crate::management::ApiState;

impl PluginService {
    pub async fn sideload_dev_package(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        cmplugin_bytes: Vec<u8>,
    ) -> Result<(bool, PluginDetailView), PluginServiceError> {
        require_foreground_write_admitted(state)?;
        ensure_dev_upload_allowed(state, &cmplugin_bytes).await?;
        let body_digest: [u8; 32] = Sha256::digest(&cmplugin_bytes).into();
        let validated = self.validate_dev_package(state, &cmplugin_bytes).await?;
        let plugin_id = validated.manifest.id.clone();

        let existing = read_existing_dev_target(state, &plugin_id).await?;
        if let Some((_, ref active_ver)) = existing
            && active_ver
                .as_ref()
                .is_some_and(|v| v.signature_status != "development")
        {
            return Err(PluginServiceError::conflict(
                "plugin.conflict",
                "cannot dev-sideload over non-development plugin",
            ));
        }

        let pin = Arc::new(self.staging_pins().pin(&validated.package_sha256_hex));
        let apply_res = async {
            let version_meta =
                stage_dev_package_artifact(state, cmplugin_bytes, &validated, Arc::clone(&pin))
                    .await?;
            let grants: Vec<PluginGrant> = manifest_required_capabilities(&validated.manifest)
                .into_iter()
                .map(|capability| PluginGrant {
                    capability,
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                })
                .collect();
            match existing {
                None => {
                    install_fresh_dev_plugin(
                        state,
                        actor_user_id,
                        &validated,
                        &version_meta,
                        &grants,
                        body_digest,
                        Arc::clone(&pin),
                    )
                    .await?;
                    Ok::<bool, PluginServiceError>(true)
                }
                Some((rec, active_ver)) => {
                    self.reload_existing_dev_plugin(
                        state,
                        actor_user_id,
                        rec,
                        active_ver,
                        &validated,
                        &version_meta,
                        &grants,
                        body_digest,
                        Arc::clone(&pin),
                    )
                    .await?;
                    Ok(false)
                }
            }
        }
        .await;
        drop(pin);

        let created = match apply_res {
            Ok(created) => {
                if !created {
                    let s_gc = state.clone();
                    let _ = run_blocking(move || {
                        gc_unreferenced_artifacts_with_store(
                            &s_gc.store,
                            s_gc.plugins.staging_pins(),
                        )
                    })
                    .await;
                }
                created
            }
            Err(err) => {
                let s_gc = state.clone();
                let _ = run_blocking(move || {
                    gc_unreferenced_artifacts_with_store(&s_gc.store, s_gc.plugins.staging_pins())
                })
                .await;
                return Err(err);
            }
        };

        let detail = self.get_plugin(state, &plugin_id).await?;
        Ok((created, detail))
    }

    async fn validate_dev_package(
        &self,
        state: &ApiState,
        cmplugin_bytes: &[u8],
    ) -> Result<plugin_package::ValidatedPackage, PluginServiceError> {
        let host = self
            .wasm_host()
            .map_err(|e| PluginServiceError::invalid("plugin.invalid_component", e.to_string()))?
            .clone();
        let s = state.clone();
        let raw = cmplugin_bytes.to_vec();
        run_blocking(move || {
            let mut policy = build_package_policy_from_store(&s.store, None)?;
            policy.dev_mode = true;
            let validated = validate_cmplugin_bytes(&raw, &policy).map_err(map_package_err)?;
            compile_validated_package_components(&host, &validated).map_err(|e| {
                PluginServiceError::invalid("plugin.invalid_component", e.to_string())
            })?;
            Ok::<_, PluginServiceError>(validated)
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn reload_existing_dev_plugin(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        rec: PluginRecord,
        active_ver: Option<PluginVersionMetadata>,
        validated: &plugin_package::ValidatedPackage,
        version_meta: &PluginVersionMetadata,
        grants: &[PluginGrant],
        body_digest: [u8; 32],
        pin: Arc<StagingPinGuard>,
    ) -> Result<(), PluginServiceError> {
        if active_ver
            .as_ref()
            .is_some_and(|v| v.signature_status != "development")
        {
            return Err(PluginServiceError::conflict(
                "plugin.conflict",
                "cannot dev-sideload over non-development plugin",
            ));
        }
        if rec.package_hash == validated.package_sha256_hex {
            return Ok(());
        }
        let plugin_id = validated.manifest.id.clone();
        if rec.lifecycle == PluginLifecycle::Enabled {
            let _ = self
                .disable_plugin(
                    state,
                    &plugin_id,
                    DisablePluginInput {
                        actor_user_id,
                        reason: Some("dev-reload".into()),
                        expected_revision: rec.revision,
                        idempotency_key: format!("dev-drain-{}", Uuid::new_v4()),
                        request: CanonicalPluginRequest {
                            method: "POST".into(),
                            path: "/plugins/dev".into(),
                            body_digest,
                        },
                    },
                )
                .await?;
        }
        replace_existing_dev_plugin(state, validated, version_meta, grants, pin).await?;
        self.cache().evict_plugin(&plugin_id);
        let s_sync = state.clone();
        let pid_sync = plugin_id;
        let _ = run_blocking(move || {
            let _ = sync_all_plugin_tasks(&s_sync, &pid_sync);
            let _ = super::registry_sync::sync_registry_snapshot(&s_sync, true);
            Ok(())
        })
        .await;
        Ok(())
    }
}

async fn stage_dev_package_artifact(
    state: &ApiState,
    cmplugin_bytes: Vec<u8>,
    validated: &plugin_package::ValidatedPackage,
    pin: Arc<StagingPinGuard>,
) -> Result<PluginVersionMetadata, PluginServiceError> {
    let s = state.clone();
    let val = validated.clone();
    run_blocking(move || {
        let _pin = pin;
        let artifact_rel =
            materialize_immutable_package_sync(s.store.data_dir(), &cmplugin_bytes, None, &val)?;
        Ok(version_metadata_from_validated(&val, artifact_rel))
    })
    .await
}

async fn ensure_dev_upload_allowed(
    state: &ApiState,
    cmplugin_bytes: &[u8],
) -> Result<(), PluginServiceError> {
    let dev = run_app_read(state, |store| Ok(store.plugin_developer_mode()?)).await?;
    if !dev.enabled {
        return Err(PluginServiceError::forbidden(
            "plugin.developer_mode_disabled",
            "developer mode must be enabled for dev sideload",
        ));
    }
    if cmplugin_bytes.is_empty() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_package",
            "empty plugin package payload",
        ));
    }
    let limits = ArchiveLimits::default();
    if (cmplugin_bytes.len() as u64) > limits.max_package_bytes {
        return Err(map_package_err(PackageError::LimitExceeded(
            "package exceeds 64 MiB limit".into(),
        )));
    }
    Ok(())
}

async fn read_existing_dev_target(
    state: &ApiState,
    plugin_id: &str,
) -> Result<Option<(PluginRecord, Option<PluginVersionMetadata>)>, PluginServiceError> {
    let pid = plugin_id.to_string();
    run_app_read(state, move |store| match store.get_plugin(&pid)? {
        None => Ok(None),
        Some(rec) => {
            let ver = store.get_plugin_version(&pid, &rec.package_hash)?;
            Ok(Some((rec, ver)))
        }
    })
    .await
}

fn ensure_dev_mode_enabled_in_app(app: &store::App<'_>) -> Result<(), PluginServiceError> {
    if !app.plugin_developer_mode()?.enabled {
        return Err(PluginServiceError::forbidden(
            "plugin.developer_mode_disabled",
            "developer mode must be enabled for dev sideload",
        ));
    }
    Ok(())
}

async fn install_fresh_dev_plugin(
    state: &ApiState,
    actor_user_id: UserId,
    validated: &plugin_package::ValidatedPackage,
    version_meta: &PluginVersionMetadata,
    grants: &[PluginGrant],
    body_digest: [u8; 32],
    pin: Arc<StagingPinGuard>,
) -> Result<(), PluginServiceError> {
    let pid = version_meta.plugin_id.clone();
    let pkg_hash = version_meta.package_hash.clone();
    let ver = version_meta.version.clone();
    let artifact = version_meta.artifact_path.clone();
    let grants_vec = grants.to_vec();
    let st_guard = state.clone();
    let (mut claim, _active_op_guard) = run_app_write_tx(state, move |app| {
        ensure_dev_mode_enabled_in_app(app)?;
        let accepted = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor_user_id),
            idempotency_key: format!("dev-install-{pid}-{}", Uuid::new_v4()),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/dev".into(),
                body_digest,
            },
            op_kind: StagedPluginOpKind::Install,
            expected_revision: 0,
            intent: StagedPluginIntent {
                target_plugin_id: pid.clone(),
                package_hash: Some(pkg_hash),
                version: Some(ver),
                artifact_path: Some(artifact),
                state_policy: None,
                target_config_digest: None,
                grants: grants_vec,
            },
            cause: PluginCommandCause::root(format!("dev-install:{pid}")),
        })?;
        let claim = app.claim_staged_plugin_operation(
            accepted.operation.operation_id,
            accepted.operation.revision,
            0,
            now_unix(),
        )?;
        let guard = st_guard.plugins.try_acquire_staged_op(claim.operation_id());
        Ok((claim, guard))
    })
    .await?;

    let op_id = claim.operation_id();
    let s = state.clone();
    let vmeta = version_meta.clone();
    let gvec = grants.to_vec();
    let manifest = validated.manifest.clone();
    run_blocking(move || {
        let _guard = _active_op_guard;
        let _pin = pin;
        let tx_res = super::service::run_foreground_app_tx(&s, true, |app| {
            ensure_dev_mode_enabled_in_app(app)?;
            let now = now_unix();
            let record = app.register_staged_plugin_version(&mut claim, &vmeta, &gvec, now)?;
            super::install_ops::seed_manifest_task_bindings_in_app(app, &record, &manifest)?;
            let res = serde_json::json!({
                "plugin_id": record.plugin_id,
                "package_hash": record.package_hash,
                "version": vmeta.version,
                "lifecycle": record.lifecycle,
            });
            app.complete_staged_plugin_operation(
                &claim,
                PluginOperationState::Succeeded,
                "installed",
                Some(&res),
                None,
                now,
            )?;
            Ok::<_, PluginServiceError>(record.generation)
        });
        match tx_res {
            Ok(installed_gen) => {
                s.plugins
                    .in_flight()
                    .close_dispatch(&vmeta.plugin_id, installed_gen.saturating_add(1));
                Ok(())
            }
            Err(err) => {
                if !super::recovery::is_transient_or_admission_err(&err) {
                    let _ = super::op_settlement::settle_failed_staged_operation_mode(
                        &s,
                        op_id,
                        "dev-install-failed",
                        true,
                    );
                }
                Err(err)
            }
        }
    })
    .await
}

async fn replace_existing_dev_plugin(
    state: &ApiState,
    validated: &plugin_package::ValidatedPackage,
    version_meta: &PluginVersionMetadata,
    grants: &[PluginGrant],
    pin: Arc<StagingPinGuard>,
) -> Result<(), PluginServiceError> {
    let vmeta = version_meta.clone();
    let pid = version_meta.plugin_id.clone();
    let gvec = grants.to_vec();
    let manifest = validated.manifest.clone();
    let updated_gen = run_app_write_tx(state, move |app| {
        let _pin = pin;
        ensure_dev_mode_enabled_in_app(app)?;
        let updated = app.replace_dev_plugin_version(&vmeta, &gvec)?;
        seed_manifest_task_bindings_in_app(app, &updated, &manifest)?;
        Ok(updated.generation)
    })
    .await?;
    state
        .plugins
        .in_flight()
        .close_dispatch(&pid, updated_gen.saturating_add(1));
    Ok(())
}
