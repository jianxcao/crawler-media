use domain::UserId;
use plugin_package::{PluginManifest, parse_minisign_public_key};
use store::plugins::{PinPluginTrustKey, PluginGrant, PluginLifecycle, PluginSubject};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    check_idempotent_replay, check_idempotent_replay_after_write, is_consent_stale_err,
    is_reauth_invalid_err, validate_idempotency_input,
};
use super::install_ops::{
    manifest_required_capabilities, validate_grant_targets, validate_grants_within_manifest,
};
use super::reauth::{ConsentPayload, digest_grants};
use super::service::{PluginService, run_app_read};
use super::types::{
    CreateTrustKeyInput, EnableDeveloperModeInput, GrantDiffView, GrantView, PagedView,
    PermissionsPreparationView, PermissionsView, PluginDetailView, PluginServiceError,
    PluginSettingsView, PluginSummaryView, PreparationImpactView, PreparePermissionsInput,
    PrepareTrustKeyInput, ReauthPurpose, TrustKeyPreparationView, TrustKeyView, format_rfc3339,
    map_package_err,
};
use crate::management::ApiState;

fn trust_key_view(
    k: &store::plugins::PluginTrustKey,
    associated_package_count: i64,
) -> TrustKeyView {
    TrustKeyView {
        key_id: k.key_id.clone(),
        label: k.label.clone(),
        public_key: k.public_key.clone(),
        fingerprint: k.fingerprint.clone(),
        status: k.status,
        revision: k.revision,
        associated_package_count,
        created_at: format_rfc3339(k.created_at),
        created_by: k.created_by.clone(),
        revoked_at: k.revoked_at.map(format_rfc3339),
        revoked_by: k.revoked_by.clone(),
        revoked_reason: k.revoked_reason.clone(),
    }
}

fn manifest_from_version(
    version: &store::plugins::PluginVersionMetadata,
) -> Option<PluginManifest> {
    if version.manifest_json == "{}" {
        None
    } else {
        serde_json::from_str(&version.manifest_json).ok()
    }
}

fn health_label(p: &store::plugins::PluginRecord) -> String {
    if p.failure_reason.is_some() {
        "degraded".into()
    } else if p.lifecycle == PluginLifecycle::Enabled {
        "healthy".into()
    } else {
        "stopped".into()
    }
}

struct VerifiedTrustKeyConsent {
    key_id: String,
    fingerprint: String,
    public_key: String,
    final_label: String,
}

async fn verify_trust_key_consent_and_reauth(
    service: &PluginService,
    state: &ApiState,
    actor_user_id: UserId,
    input: &CreateTrustKeyInput,
) -> Result<Result<VerifiedTrustKeyConsent, TrustKeyView>, PluginServiceError> {
    if let Err(err) = service
        .peek_reauth_token(
            state,
            actor_user_id,
            ReauthPurpose::TrustKeyCreate,
            &input.reauth_token,
        )
        .await
    {
        if is_reauth_invalid_err(&err)
            && let Some(replayed) = check_idempotent_replay_after_write::<TrustKeyView>(
                state,
                actor_user_id,
                "trust-key-create",
                &input.idempotency_key,
                &input.request,
            )
            .await?
        {
            return Ok(Err(replayed));
        }
        return Err(err);
    }

    let pk_trim = input.public_key.trim().to_string();
    let fp_trim = input.fingerprint.trim().to_ascii_lowercase();
    let payload = match service
        .consent()
        .peek(actor_user_id, None, &input.consent_token, |p| match p {
            ConsentPayload::TrustKey {
                public_key,
                fingerprint,
                ..
            } if public_key == &pk_trim && fingerprint == &fp_trim => Ok(p.clone()),
            _ => Err(PluginServiceError::conflict(
                "plugin.consent_stale",
                "trust key consent_token mismatch",
            )),
        }) {
        Ok(p) => p,
        Err(err) => {
            if is_consent_stale_err(&err)
                && let Some(replayed) = check_idempotent_replay_after_write::<TrustKeyView>(
                    state,
                    actor_user_id,
                    "trust-key-create",
                    &input.idempotency_key,
                    &input.request,
                )
                .await?
            {
                return Ok(Err(replayed));
            }
            return Err(err);
        }
    };
    let ConsentPayload::TrustKey {
        key_id,
        fingerprint,
        public_key,
        label: prepared_label,
    } = payload
    else {
        return Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "invalid trust key consent",
        ));
    };
    let final_label = input
        .label
        .clone()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or(prepared_label);
    Ok(Ok(VerifiedTrustKeyConsent {
        key_id,
        fingerprint,
        public_key,
        final_label,
    }))
}

impl PluginService {
    pub async fn list_trust_keys(
        &self,
        state: &ApiState,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<PagedView<TrustKeyView>, PluginServiceError> {
        run_app_read(state, move |store| {
            let page = store.list_plugin_trust_keys(cursor.as_deref(), limit)?;
            Ok(PagedView {
                items: page
                    .items
                    .iter()
                    .map(|s| trust_key_view(&s.key, s.associated_package_count))
                    .collect(),
                next_cursor: page.next_cursor,
            })
        })
        .await
    }

    pub async fn prepare_trust_key(
        &self,
        _state: &ApiState,
        actor_user_id: UserId,
        input: PrepareTrustKeyInput,
    ) -> Result<TrustKeyPreparationView, PluginServiceError> {
        if input.label.trim().is_empty() || input.label.len() > 256 {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "trust key label must be 1..=256 characters",
            ));
        }
        let parsed = parse_minisign_public_key(&input.public_key).map_err(map_package_err)?;
        let preparation_id = Uuid::new_v4();
        let label = input.label.trim().to_string();
        let (consent_token, expires_at) = self.consent().mint(
            actor_user_id,
            preparation_id,
            ConsentPayload::TrustKey {
                key_id: parsed.key_id.clone(),
                fingerprint: parsed.fingerprint.clone(),
                public_key: input.public_key.trim().to_string(),
                label: label.clone(),
            },
        );
        Ok(TrustKeyPreparationView {
            preparation_id,
            key_id: parsed.key_id,
            label,
            public_key: input.public_key.trim().to_string(),
            fingerprint: parsed.fingerprint,
            consent_token,
            expires_at,
        })
    }

    pub async fn create_trust_key(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        input: CreateTrustKeyInput,
    ) -> Result<TrustKeyView, PluginServiceError> {
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_idempotent_replay::<TrustKeyView>(
            state,
            actor_user_id,
            "trust-key-create",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }
        if input.expected_revision != 0 {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "creating a trust key requires expected_revision 0",
            ));
        }
        let verified =
            match verify_trust_key_consent_and_reauth(self, state, actor_user_id, &input).await? {
                Ok(v) => v,
                Err(replayed) => return Ok(replayed),
            };
        let consent = self.consent().clone();
        let consent_tok = input.consent_token;
        let reauth = self.reauth().clone();
        let reauth_tok = input.reauth_token;
        let pk_exp = input.public_key.trim().to_string();
        let fp_exp = input.fingerprint.trim().to_ascii_lowercase();
        let idem_retry = input.idempotency_key.clone();
        let req_retry = input.request.clone();
        let idem = input.idempotency_key;
        let req = input.request;
        let write_res = run_app_write_tx(state, move |app| {
            if let Some(val) = app.get_plugin_idempotency_envelope(
                &PluginSubject::User(actor_user_id),
                "trust-key-create",
                &idem,
                &req,
            )? {
                return serde_json::from_value(val)
                    .map_err(|e| PluginServiceError::Io(e.to_string()));
            }
            let check = |p: &ConsentPayload| match p {
                ConsentPayload::TrustKey {
                    public_key,
                    fingerprint,
                    ..
                } if public_key == &pk_exp && fingerprint == &fp_exp => Ok(()),
                _ => Err(PluginServiceError::conflict(
                    "plugin.consent_stale",
                    "trust key consent_token mismatch",
                )),
            };
            reauth.peek_in_app_tx(
                app,
                actor_user_id,
                ReauthPurpose::TrustKeyCreate,
                &reauth_tok,
            )?;
            consent.peek(actor_user_id, None, &consent_tok, check)?;
            let created = app.pin_plugin_trust_key(&PinPluginTrustKey {
                key_id: verified.key_id.clone(),
                label: verified.final_label,
                public_key: verified.public_key,
                fingerprint: verified.fingerprint,
                actor: actor_user_id.to_string(),
            })?;
            reauth.consume_in_app_tx(
                app,
                actor_user_id,
                ReauthPurpose::TrustKeyCreate,
                &reauth_tok,
            )?;
            let _ = consent.peek_consume(actor_user_id, None, &consent_tok, check)?;
            let count = app.count_plugin_versions_for_key(&verified.key_id)?;
            let view = trust_key_view(&created, count);
            let json =
                serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            app.put_plugin_idempotency_envelope(
                &PluginSubject::User(actor_user_id),
                "trust-key-create",
                &idem,
                &req,
                &json,
            )?;
            Ok(view)
        })
        .await;
        match write_res {
            Ok(view) => Ok(view),
            Err(err) => {
                if (is_reauth_invalid_err(&err)
                    || is_consent_stale_err(&err)
                    || matches!(
                        err,
                        PluginServiceError::Conflict { .. } | PluginServiceError::Store(_)
                    ))
                    && let Some(replayed) = check_idempotent_replay_after_write::<TrustKeyView>(
                        state,
                        actor_user_id,
                        "trust-key-create",
                        &idem_retry,
                        &req_retry,
                    )
                    .await?
                {
                    return Ok(replayed);
                }
                Err(err)
            }
        }
    }

    pub async fn get_plugin_settings(
        &self,
        state: &ApiState,
    ) -> Result<PluginSettingsView, PluginServiceError> {
        run_app_read(state, |store| {
            let dev = store.plugin_developer_mode()?;
            Ok(PluginSettingsView {
                developer_mode: dev.enabled,
                revision: dev.revision,
                updated_at: dev.updated_at.map(format_rfc3339),
                updated_by: dev.updated_by,
            })
        })
        .await
    }

    pub async fn enable_developer_mode(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        input: EnableDeveloperModeInput,
    ) -> Result<PluginSettingsView, PluginServiceError> {
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_idempotent_replay::<PluginSettingsView>(
            state,
            actor_user_id,
            "developer-mode-enable",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }
        let reauth = self.reauth().clone();
        let reauth_tok = input.reauth_token;
        let rev = input.expected_revision;
        let idem = input.idempotency_key;
        let req = input.request;
        run_app_write_tx(state, move |app| {
            if let Some(val) = app.get_plugin_idempotency_envelope(
                &PluginSubject::User(actor_user_id),
                "developer-mode-enable",
                &idem,
                &req,
            )? {
                return serde_json::from_value(val)
                    .map_err(|e| PluginServiceError::Io(e.to_string()));
            }
            reauth.peek_in_app_tx(
                app,
                actor_user_id,
                ReauthPurpose::DeveloperModeEnable,
                &reauth_tok,
            )?;
            let dev = app.set_plugin_developer_mode(true, rev, &actor_user_id.to_string())?;
            reauth.consume_in_app_tx(
                app,
                actor_user_id,
                ReauthPurpose::DeveloperModeEnable,
                &reauth_tok,
            )?;
            let view = PluginSettingsView {
                developer_mode: dev.enabled,
                revision: dev.revision,
                updated_at: dev.updated_at.map(format_rfc3339),
                updated_by: dev.updated_by,
            };
            let json =
                serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            app.put_plugin_idempotency_envelope(
                &PluginSubject::User(actor_user_id),
                "developer-mode-enable",
                &idem,
                &req,
                &json,
            )?;
            Ok(view)
        })
        .await
    }

    pub async fn list_plugins(
        &self,
        state: &ApiState,
        lifecycle: Option<PluginLifecycle>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<PagedView<PluginSummaryView>, PluginServiceError> {
        run_app_read(state, move |store| {
            let page = store.list_plugins_page(lifecycle, cursor.as_deref(), limit)?;
            let mut items = Vec::with_capacity(page.items.len());
            for p in &page.items {
                let ver = store.get_plugin_version(&p.plugin_id, &p.package_hash)?;
                let manifest = ver.as_ref().and_then(manifest_from_version);
                let grants = store.get_plugin_grants(&p.plugin_id)?;
                items.push(PluginSummaryView {
                    id: p.plugin_id.clone(),
                    name: manifest
                        .as_ref()
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| p.plugin_id.clone()),
                    version: ver
                        .as_ref()
                        .map(|v| v.version.clone())
                        .unwrap_or_else(|| "0.0.0".into()),
                    package_hash: p.package_hash.clone(),
                    source: ver
                        .as_ref()
                        .map(|v| v.signature_status.clone())
                        .unwrap_or_else(|| "unknown".into()),
                    revision: p.revision,
                    config_revision: p.config_revision,
                    generation: p.generation,
                    enabled: p.desired_enabled && p.lifecycle == PluginLifecycle::Enabled,
                    lifecycle: p.lifecycle,
                    health: health_label(p),
                    required_grants: manifest
                        .as_ref()
                        .map(manifest_required_capabilities)
                        .unwrap_or_default(),
                    effective_grants: grants,
                    active_operation_id: p.active_operation_id,
                    failure_reason: p.failure_reason.clone(),
                });
            }
            Ok(PagedView {
                items,
                next_cursor: page.next_cursor,
            })
        })
        .await
    }

    pub async fn get_plugin(
        &self,
        state: &ApiState,
        plugin_id: &str,
    ) -> Result<PluginDetailView, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "active plugin version missing")
                })?;
            let manifest = manifest_from_version(&ver);
            let grants = store.get_plugin_grants(&pid)?;
            let tasks = store.list_plugin_task_bindings(&pid, None, Some(200))?;
            let versions = store.list_plugin_versions(&pid, None, Some(200))?;
            Ok(PluginDetailView {
                id: p.plugin_id.clone(),
                name: manifest
                    .as_ref()
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| p.plugin_id.clone()),
                version: ver.version,
                package_hash: p.package_hash.clone(),
                source: ver.signature_status,
                signing_key_id: ver.signing_key_id,
                revision: p.revision,
                config_revision: p.config_revision,
                generation: p.generation,
                enabled: p.desired_enabled && p.lifecycle == PluginLifecycle::Enabled,
                lifecycle: p.lifecycle,
                health: health_label(&p),
                required_grants: manifest
                    .as_ref()
                    .map(manifest_required_capabilities)
                    .unwrap_or_default(),
                effective_grants: grants,
                active_operation_id: p.active_operation_id,
                failure_reason: p.failure_reason,
                state_schema: ver.state_schema.parse().unwrap_or(0),
                credential_bound: ver.credential_bound,
                task_count: tasks.items.len(),
                version_count: versions.items.len(),
            })
        })
        .await
    }

    pub async fn get_permissions(
        &self,
        state: &ApiState,
        plugin_id: &str,
    ) -> Result<PermissionsView, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let required_capabilities = manifest_from_version(&ver)
                .as_ref()
                .map(manifest_required_capabilities)
                .unwrap_or_default();
            let records = store.list_plugin_grants(&pid, &p.package_hash)?;
            Ok(PermissionsView {
                plugin_id: pid,
                package_hash: p.package_hash,
                revision: p.revision,
                required_capabilities,
                effective_grants: records
                    .into_iter()
                    .map(|r| GrantView {
                        capability: r.grant.capability,
                        scope: r.grant.scope,
                        sensitive: r.grant.sensitive,
                        status: r.status,
                    })
                    .collect(),
            })
        })
        .await
    }

    pub async fn prepare_permissions(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        input: PreparePermissionsInput,
    ) -> Result<PermissionsPreparationView, PluginServiceError> {
        let pid = plugin_id.to_string();
        let req_grants = input.requested_grants.clone();
        let (plugin, current_grants, required) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let ver = store
                .get_plugin_version(&pid, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let required = if ver.manifest_json == "{}" {
                Vec::new()
            } else {
                let manifest = serde_json::from_str::<PluginManifest>(&ver.manifest_json)
                    .map_err(|e| PluginServiceError::Io(e.to_string()))?;
                let req = manifest_required_capabilities(&manifest);
                validate_grants_within_manifest(&req_grants, &req)?;
                req
            };
            validate_grant_targets(store, &req_grants)?;
            let current = store.get_plugin_grants(&pid)?;
            Ok((p, current, required))
        })
        .await?;

        let added: Vec<PluginGrant> = input
            .requested_grants
            .iter()
            .filter(|g| !current_grants.contains(g))
            .cloned()
            .collect();
        let kept: Vec<PluginGrant> = input
            .requested_grants
            .iter()
            .filter(|g| current_grants.contains(g))
            .cloned()
            .collect();
        let removed: Vec<PluginGrant> = current_grants
            .iter()
            .filter(|g| !input.requested_grants.contains(g))
            .cloned()
            .collect();

        let grants_digest = digest_grants(&input.requested_grants)?;
        let preparation_id = Uuid::new_v4();
        let (consent_token, expires_at) = self.consent().mint(
            actor_user_id,
            preparation_id,
            ConsentPayload::Permissions {
                plugin_id: plugin.plugin_id.clone(),
                package_hash: plugin.package_hash.clone(),
                base_revision: plugin.revision,
                grants_digest,
            },
        );
        Ok(PermissionsPreparationView {
            preparation_id,
            plugin_id: plugin.plugin_id,
            package_hash: plugin.package_hash,
            base_revision: plugin.revision,
            grant_diff: GrantDiffView {
                added,
                kept,
                removed,
                required,
            },
            impact: PreparationImpactView {
                summary: format!(
                    "Grant {} permission(s) to {plugin_id}",
                    input.requested_grants.len()
                ),
                requires_drain: plugin.lifecycle == PluginLifecycle::Enabled,
                requires_reauth: false,
                resets_state: false,
            },
            consent_token,
            expires_at,
        })
    }
}
