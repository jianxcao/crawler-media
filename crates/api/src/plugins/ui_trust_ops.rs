use std::sync::Arc;

use domain::UserId;
use plugin_package::UiSurfaceKind;
use serde::{Deserialize, Serialize};
use store::plugins::{
    PluginEffectObservation, PluginError, PluginOperationState, PluginRecord, PluginSubject,
    PluginUiTrustRecord, PluginVersionMetadata, RecordPluginAudit,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{is_consent_stale_err, is_reauth_invalid_err, validate_idempotency_input};
use super::reauth::{ConsentPayload, ConsentStore, ReauthService, now_unix};
use super::service::{PluginService, run_app_read};
use super::types::{PluginServiceError, ReauthPurpose, format_rfc3339};
use super::ui_ops::{parse_manifest_with_ui, resolve_bundle_sha256};
use crate::management::ApiState;

#[derive(Clone, Serialize, Deserialize)]
pub struct UiTrustPreparationView {
    pub preparation_id: Uuid,
    pub plugin_id: String,
    pub package_hash: String,
    pub package_sha256: String,
    pub bundle_sha256: String,
    pub remote_name: String,
    pub capabilities: Vec<String>,
    pub surfaces: Vec<UiSurfacePrepView>,
    pub trust_revision: i64,
    pub consent_token: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiSurfacePrepView {
    pub id: String,
    pub kind: UiSurfaceKind,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiTrustActionView {
    pub plugin_id: String,
    pub package_hash: String,
    pub package_sha256: String,
    pub bundle_sha256: String,
    pub status: String,
    pub ui_trusted: bool,
    pub revision: i64,
    pub granted_by_user_id: String,
    pub granted_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Clone)]
pub struct GrantUiTrustInput {
    pub preparation_id: Option<Uuid>,
    pub package_hash: Option<String>,
    pub consent_token: String,
    pub reauth_token: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: store::plugins::CanonicalPluginRequest,
}

#[derive(Clone)]
pub struct RevokeUiTrustInput {
    pub package_hash: Option<String>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: store::plugins::CanonicalPluginRequest,
}

fn trust_action_view(rec: &PluginUiTrustRecord) -> UiTrustActionView {
    let trusted = rec.revoked_at.is_none();
    UiTrustActionView {
        plugin_id: rec.plugin_id.clone(),
        package_hash: rec.package_sha256.clone(),
        package_sha256: rec.package_sha256.clone(),
        bundle_sha256: rec.bundle_sha256.clone(),
        status: if trusted {
            "trusted".into()
        } else {
            "revoked".into()
        },
        ui_trusted: trusted,
        revision: rec.revision,
        granted_by_user_id: rec.granted_by_user_id.clone(),
        granted_at: rec.granted_at.clone(),
        revoked_at: rec.revoked_at.clone(),
    }
}

fn validate_ui_trust_consent(
    payload: &ConsentPayload,
    plugin_id: &str,
    package_hash: &str,
    expected_revision: i64,
) -> Result<String, PluginServiceError> {
    match payload {
        ConsentPayload::UiTrust {
            plugin_id: cp,
            package_sha256,
            bundle_sha256,
            expected_revision: exp_rev,
        } if cp == plugin_id && package_sha256 == package_hash && *exp_rev == expected_revision => {
            Ok(bundle_sha256.clone())
        }
        _ => Err(PluginServiceError::conflict(
            "plugin.consent_stale",
            "consent_token does not match plugin UI trust state",
        )),
    }
}

async fn load_plugin_ui_trust_snapshot(
    state: &ApiState,
    plugin_id: &str,
) -> Result<
    (
        PluginRecord,
        PluginVersionMetadata,
        Option<PluginUiTrustRecord>,
    ),
    PluginServiceError,
> {
    let pid = plugin_id.to_string();
    run_app_read(state, move |store| {
        let p = store
            .get_plugin(&pid)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        let ver = store
            .get_plugin_version(&pid, &p.package_hash)?
            .ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin version not found")
            })?;
        let trust = store.get_ui_trust(&pid, &p.package_hash)?;
        Ok((p, ver, trust))
    })
    .await
}

impl PluginService {
    pub(crate) async fn prepare_ui_trust(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        requested_package_hash: Option<&str>,
    ) -> Result<UiTrustPreparationView, PluginServiceError> {
        let (plugin, version, trust_row) = load_plugin_ui_trust_snapshot(state, plugin_id).await?;
        if let Some(req_hash) = requested_package_hash
            && req_hash != plugin.package_hash
        {
            return Err(PluginServiceError::conflict(
                "plugin.package_conflict",
                "package_hash does not match active plugin package",
            ));
        }
        let (_, ui) = parse_manifest_with_ui(&version)?;
        let bundle_sha256 = resolve_bundle_sha256(state.store.data_dir(), &version, &ui).await?;
        let trust_revision = trust_row.map_or(0, |r| r.revision);
        let preparation_id = Uuid::new_v4();
        let (consent_token, expires_at) = self.consent().mint(
            actor_user_id,
            preparation_id,
            ConsentPayload::UiTrust {
                plugin_id: plugin.plugin_id.clone(),
                package_sha256: plugin.package_hash.clone(),
                bundle_sha256: bundle_sha256.clone(),
                expected_revision: trust_revision,
            },
        );
        Ok(UiTrustPreparationView {
            preparation_id,
            plugin_id: plugin.plugin_id,
            package_hash: plugin.package_hash.clone(),
            package_sha256: plugin.package_hash,
            bundle_sha256,
            remote_name: ui.remote_name,
            capabilities: ui.capabilities,
            surfaces: ui
                .surfaces
                .into_iter()
                .map(|s| UiSurfacePrepView {
                    id: s.id,
                    kind: s.kind,
                    title: s.title,
                })
                .collect(),
            trust_revision,
            consent_token,
            expires_at,
        })
    }

    pub(crate) async fn grant_ui_trust(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        input: GrantUiTrustInput,
    ) -> Result<UiTrustActionView, PluginServiceError> {
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_ui_trust_replay(
            state,
            actor_user_id,
            "ui-trust-grant",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }
        let (plugin, version, trust_row) = load_plugin_ui_trust_snapshot(state, plugin_id).await?;
        let _ = parse_manifest_with_ui(&version)?;
        if let Some(req_hash) = input.package_hash.as_deref()
            && req_hash != plugin.package_hash
        {
            return Err(PluginServiceError::conflict(
                "plugin.package_conflict",
                "package_hash does not match active plugin package",
            ));
        }
        if input.expected_revision != trust_row.map_or(0, |r| r.revision) {
            if let Some(replayed) = check_ui_trust_replay_after_write(
                state,
                actor_user_id,
                "ui-trust-grant",
                &input.idempotency_key,
                &input.request,
            )
            .await?
            {
                return Ok(replayed);
            }
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "UI trust revision conflict",
            ));
        }
        let bundle_sha256 = match self
            .preflight_grant_auth(
                state,
                actor_user_id,
                plugin_id,
                &plugin.package_hash,
                &input,
            )
            .await?
        {
            Ok(sha) => sha,
            Err(replayed) => return Ok(replayed),
        };
        commit_ui_trust_grant(
            state,
            self.consent().clone(),
            self.reauth().clone(),
            actor_user_id,
            plugin.plugin_id,
            plugin.package_hash,
            bundle_sha256,
            input,
        )
        .await
    }

    async fn preflight_grant_auth(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        package_hash: &str,
        input: &GrantUiTrustInput,
    ) -> Result<Result<String, UiTrustActionView>, PluginServiceError> {
        match self
            .peek_grant_auth(state, actor_user_id, plugin_id, package_hash, input)
            .await
        {
            Ok(sha) => Ok(Ok(sha)),
            Err(err) => {
                if (is_reauth_invalid_err(&err)
                    || is_consent_stale_err(&err)
                    || matches!(
                        err,
                        PluginServiceError::Forbidden { .. } | PluginServiceError::Conflict { .. }
                    ))
                    && let Some(replayed) = check_ui_trust_replay_after_write(
                        state,
                        actor_user_id,
                        "ui-trust-grant",
                        &input.idempotency_key,
                        &input.request,
                    )
                    .await?
                {
                    return Ok(Err(replayed));
                }
                Err(err)
            }
        }
    }

    pub(crate) async fn revoke_ui_trust(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        input: RevokeUiTrustInput,
    ) -> Result<UiTrustActionView, PluginServiceError> {
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_ui_trust_replay(
            state,
            actor_user_id,
            "ui-trust-revoke",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }
        commit_ui_trust_revoke(state, actor_user_id, plugin_id.to_string(), input).await
    }

    async fn peek_grant_auth(
        &self,
        state: &ApiState,
        actor_user_id: UserId,
        plugin_id: &str,
        package_hash: &str,
        input: &GrantUiTrustInput,
    ) -> Result<String, PluginServiceError> {
        self.reauth()
            .peek(
                state,
                actor_user_id,
                ReauthPurpose::PluginUiTrust,
                &input.reauth_token,
            )
            .await?;
        self.consent().peek(
            actor_user_id,
            input.preparation_id,
            &input.consent_token,
            |payload| {
                validate_ui_trust_consent(payload, plugin_id, package_hash, input.expected_revision)
            },
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit_ui_trust_grant(
    state: &ApiState,
    consent: Arc<ConsentStore>,
    reauth: Arc<ReauthService>,
    actor_user_id: UserId,
    pid_w: String,
    pkg_hash: String,
    bundle_sha256: String,
    input: GrantUiTrustInput,
) -> Result<UiTrustActionView, PluginServiceError> {
    let granted_at = format_rfc3339(now_unix());
    let expected_rev = input.expected_revision;
    let prep_id = input.preparation_id;
    let consent_tok = input.consent_token;
    let reauth_tok = input.reauth_token;
    let idem = input.idempotency_key;
    let req = input.request;
    let idem_retry = idem.clone();
    let req_retry = req.clone();
    let res = run_app_write_tx(state, move |app| {
        let subject = PluginSubject::User(actor_user_id);
        if let Some(val) =
            app.get_plugin_idempotency_envelope(&subject, "ui-trust-grant", &idem, &req)?
        {
            return serde_json::from_value::<UiTrustActionView>(val)
                .map_err(|e| PluginServiceError::Io(e.to_string()));
        }
        let cur = app
            .get_plugin(&pid_w)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        if cur.package_hash != pkg_hash {
            return Err(PluginServiceError::conflict(
                "plugin.package_conflict",
                "package_hash does not match active plugin package",
            ));
        }
        reauth.peek_in_app_tx(
            app,
            actor_user_id,
            ReauthPurpose::PluginUiTrust,
            &reauth_tok,
        )?;
        consent.peek(actor_user_id, prep_id, &consent_tok, |payload| {
            validate_ui_trust_consent(payload, &pid_w, &pkg_hash, expected_rev).map(|_| ())
        })?;
        let saved = app.grant_ui_trust(&store::plugins::GrantPluginUiTrust {
            plugin_id: pid_w.clone(),
            package_sha256: pkg_hash.clone(),
            bundle_sha256,
            granted_by_user_id: actor_user_id.to_string(),
            granted_at,
            expected_revision: expected_rev,
        })?;
        reauth.consume_in_app_tx(
            app,
            actor_user_id,
            ReauthPurpose::PluginUiTrust,
            &reauth_tok,
        )?;
        consent.peek_consume(actor_user_id, prep_id, &consent_tok, |payload| {
            validate_ui_trust_consent(payload, &pid_w, &pkg_hash, expected_rev).map(|_| ())
        })?;
        let view = trust_action_view(&saved);
        let _ = app.append_plugin_audit(&RecordPluginAudit {
            plugin_id: pid_w.clone(),
            generation: Some(cur.generation),
            action: "ui_trust.grant".into(),
            method: req.method.clone(),
            path: req.path.clone(),
            cause_id: format!("ui-trust:{pid_w}"),
            state: PluginOperationState::Succeeded,
            phase: "granted".into(),
            effect: PluginEffectObservation::Confirmed,
        })?;
        let json =
            serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        app.put_plugin_idempotency_envelope(&subject, "ui-trust-grant", &idem, &req, &json)?;
        Ok(view)
    })
    .await;
    match res {
        Ok(view) => Ok(view),
        Err(err) => {
            if (is_reauth_invalid_err(&err)
                || is_consent_stale_err(&err)
                || matches!(
                    err,
                    PluginServiceError::Forbidden { .. }
                        | PluginServiceError::Conflict { .. }
                        | PluginServiceError::Store(store::StoreError::Plugin(
                            PluginError::Conflict
                        ))
                ))
                && let Some(replayed) = check_ui_trust_replay_after_write(
                    state,
                    actor_user_id,
                    "ui-trust-grant",
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

async fn commit_ui_trust_revoke(
    state: &ApiState,
    actor_user_id: UserId,
    pid_w: String,
    input: RevokeUiTrustInput,
) -> Result<UiTrustActionView, PluginServiceError> {
    let req_pkg_hash = input.package_hash;
    let expected_rev = input.expected_revision;
    let revoked_at = format_rfc3339(now_unix());
    let idem = input.idempotency_key;
    let req = input.request;
    let idem_retry = idem.clone();
    let req_retry = req.clone();
    let res = run_app_write_tx(state, move |app| {
        let subject = PluginSubject::User(actor_user_id);
        if let Some(val) =
            app.get_plugin_idempotency_envelope(&subject, "ui-trust-revoke", &idem, &req)?
        {
            return serde_json::from_value::<UiTrustActionView>(val)
                .map_err(|e| PluginServiceError::Io(e.to_string()));
        }
        let cur = app
            .get_plugin(&pid_w)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        let target_hash = req_pkg_hash.unwrap_or_else(|| cur.package_hash.clone());
        let revoked = app.revoke_ui_trust(&pid_w, &target_hash, expected_rev, &revoked_at)?;
        let view = trust_action_view(&revoked);
        let _ = app.append_plugin_audit(&RecordPluginAudit {
            plugin_id: pid_w.clone(),
            generation: Some(cur.generation),
            action: "ui_trust.revoke".into(),
            method: req.method.clone(),
            path: req.path.clone(),
            cause_id: format!("ui-trust:{pid_w}"),
            state: PluginOperationState::Succeeded,
            phase: "revoked".into(),
            effect: PluginEffectObservation::Confirmed,
        })?;
        let json =
            serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        app.put_plugin_idempotency_envelope(&subject, "ui-trust-revoke", &idem, &req, &json)?;
        Ok(view)
    })
    .await;
    match res {
        Ok(view) => Ok(view),
        Err(err) => {
            if matches!(
                err,
                PluginServiceError::Conflict { .. }
                    | PluginServiceError::Store(store::StoreError::Plugin(PluginError::Conflict))
            ) && let Some(replayed) = check_ui_trust_replay_after_write(
                state,
                actor_user_id,
                "ui-trust-revoke",
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

async fn check_ui_trust_replay_after_write(
    state: &ApiState,
    actor: UserId,
    op_kind: &str,
    idem: &str,
    req: &store::plugins::CanonicalPluginRequest,
) -> Result<Option<UiTrustActionView>, PluginServiceError> {
    super::idempotency::wait_app_write_idle(state).await;
    check_ui_trust_replay(state, actor, op_kind, idem, req).await
}

async fn check_ui_trust_replay(
    state: &ApiState,
    actor: UserId,
    op_kind: &str,
    idem: &str,
    req: &store::plugins::CanonicalPluginRequest,
) -> Result<Option<UiTrustActionView>, PluginServiceError> {
    let op_kind = op_kind.to_string();
    let idem = idem.to_string();
    let req = req.clone();
    run_app_read(state, move |store| {
        match store.get_plugin_idempotency_envelope(
            &PluginSubject::User(actor),
            &op_kind,
            &idem,
            &req,
        ) {
            Ok(Some(val)) => {
                let parsed = serde_json::from_value::<UiTrustActionView>(val)
                    .map_err(|e| PluginServiceError::Io(e.to_string()))?;
                Ok(Some(parsed))
            }
            Ok(None) => Ok(None),
            Err(store::StoreError::Plugin(PluginError::IdempotencyConflict)) => {
                Err(PluginServiceError::conflict(
                    "plugin.idempotency_conflict",
                    "Idempotency-Key reused with different request payload",
                ))
            }
            Err(e) => Err(e.into()),
        }
    })
    .await
}
