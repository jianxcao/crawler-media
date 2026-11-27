use domain::UserId;
use serde::de::DeserializeOwned;
use store::plugins::{CanonicalPluginRequest, PluginSubject};
use uuid::Uuid;

use super::lifecycle_ops::op_to_admin_view;
use super::service::run_app_read;
use super::types::{OperationAdminView, PluginServiceError};
use crate::management::ApiState;

pub(crate) enum OpBegin<T> {
    Proceed(T),
    Replayed(Box<OperationAdminView>),
}

pub(crate) fn validate_idempotency_input(
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
) -> Result<(), PluginServiceError> {
    Ok(store::plugins::validate_idempotency_input(
        idempotency_key,
        request,
    )?)
}

pub(crate) fn load_app_operation_admin_view(
    app: &store::App<'_>,
    op_id: Uuid,
) -> Result<OperationAdminView, PluginServiceError> {
    let admin = app.get_plugin_operation_admin(op_id)?.ok_or_else(|| {
        PluginServiceError::not_found("plugin.operation_missing", "operation not found")
    })?;
    let job_id = admin
        .result_json
        .as_ref()
        .and_then(|v| v.get("job_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(op_to_admin_view(
        &admin.operation,
        job_id,
        admin.result_json,
    ))
}

pub(crate) async fn wait_app_write_idle(state: &ApiState) {
    let store = state.store.clone();
    let _ = super::service::run_blocking(move || {
        let _ = store.wait_for_app_write_idle(std::time::Duration::from_secs(1));
        Ok(())
    })
    .await;
}

pub(crate) async fn check_install_replay(
    state: &ApiState,
    actor: UserId,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
    kind: &'static str,
) -> Result<Option<OperationAdminView>, PluginServiceError> {
    validate_idempotency_input(idempotency_key, request)?;
    let key = idempotency_key.to_string();
    let req = request.clone();
    run_app_read(state, move |store| {
        let Some(op) = store.get_plugin_operation_by_subject_idempotency(
            &PluginSubject::User(actor),
            &key,
            &req,
            kind,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(super::lifecycle_ops::load_operation_admin_view(
            store,
            op.operation_id,
        )?))
    })
    .await
}

pub(crate) async fn check_install_replay_after_write(
    state: &ApiState,
    actor: UserId,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
    kind: &'static str,
) -> Result<Option<OperationAdminView>, PluginServiceError> {
    wait_app_write_idle(state).await;
    check_install_replay(state, actor, idempotency_key, request, kind).await
}

pub(crate) async fn check_idempotent_replay<T>(
    state: &ApiState,
    actor: UserId,
    scope: &'static str,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
) -> Result<Option<T>, PluginServiceError>
where
    T: DeserializeOwned + Send + 'static,
{
    validate_idempotency_input(idempotency_key, request)?;
    let key = idempotency_key.to_string();
    let req = request.clone();
    run_app_read(state, move |store| {
        let Some(val) = store.get_plugin_idempotency_envelope(
            &PluginSubject::User(actor),
            scope,
            &key,
            &req,
        )?
        else {
            return Ok(None);
        };
        let parsed =
            serde_json::from_value(val).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        Ok(Some(parsed))
    })
    .await
}

pub(crate) async fn check_idempotent_replay_after_write<T>(
    state: &ApiState,
    actor: UserId,
    scope: &'static str,
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
) -> Result<Option<T>, PluginServiceError>
where
    T: DeserializeOwned + Send + 'static,
{
    wait_app_write_idle(state).await;
    check_idempotent_replay(state, actor, scope, idempotency_key, request).await
}

pub(crate) fn is_consent_stale_err(err: &PluginServiceError) -> bool {
    matches!(
        err,
        PluginServiceError::Conflict {
            code: "plugin.consent_stale",
            ..
        }
    )
}

pub(crate) fn is_reauth_invalid_err(err: &PluginServiceError) -> bool {
    matches!(
        err,
        PluginServiceError::Forbidden {
            code: "plugin.reauth_invalid",
            ..
        }
    )
}

#[derive(Clone, Debug)]
pub(crate) struct ResumeIdempotencyCtx {
    pub(crate) actor: UserId,
    pub(crate) idempotency_key: String,
    pub(crate) request: CanonicalPluginRequest,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ResumeClaimMeta {
    actor: String,
    idempotency_key: String,
    method: String,
    path: String,
    body_digest: [u8; 32],
    original_revision: i64,
    claimed_revision: i64,
}

const RESUME_CLAIM_PREFIX: &str = "plugin_op.resume_claim.";

fn resume_claim_setting_key(op_id: Uuid) -> String {
    format!("{RESUME_CLAIM_PREFIX}{op_id}")
}

pub(crate) fn has_active_resume_claim(store: &store::Store, op_id: Uuid) -> bool {
    store
        .get_setting(&resume_claim_setting_key(op_id))
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn clear_resume_claim_meta_in_app(
    app: &store::App<'_>,
    op_id: Uuid,
) -> Result<(), store::StoreError> {
    let _ = app.delete_setting(&resume_claim_setting_key(op_id))?;
    Ok(())
}

pub(crate) fn check_resume_claim_conflict_in_app(
    app: &store::App<'_>,
    target_op_id: Uuid,
    ctx: &ResumeIdempotencyCtx,
) -> Result<Option<ResumeClaimMeta>, PluginServiceError> {
    let actor_str = ctx.actor.to_string();
    let mut matched_meta: Option<ResumeClaimMeta> = None;
    for key in app.list_setting_keys_with_prefix(RESUME_CLAIM_PREFIX)? {
        let Some(other_id) = key
            .strip_prefix(RESUME_CLAIM_PREFIX)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        let is_live = app
            .get_plugin_operation_admin(other_id)?
            .is_some_and(|a| !a.operation.state.is_terminal());
        if !is_live {
            let _ = app.delete_setting(&key)?;
            continue;
        }
        let Some(raw) = app.get_setting(&key)? else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<ResumeClaimMeta>(&raw) else {
            continue;
        };
        if meta.actor == actor_str && meta.idempotency_key == ctx.idempotency_key {
            if other_id != target_op_id
                || meta.method != ctx.request.method
                || meta.path != ctx.request.path
                || meta.body_digest != ctx.request.body_digest
            {
                return Err(PluginServiceError::conflict(
                    "plugin.idempotency_conflict",
                    "idempotency key reused with different request",
                ));
            }
            matched_meta = Some(meta);
        }
    }
    Ok(matched_meta)
}

pub(crate) fn matches_active_resume_claim(
    op: &store::plugins::PluginOperation,
    matched_meta: Option<&ResumeClaimMeta>,
    expected_rev: i64,
) -> bool {
    if op.state != store::plugins::PluginOperationState::Running || op.phase == "cancel-requested" {
        return false;
    }
    let Some(meta) = matched_meta else {
        return false;
    };
    meta.original_revision == expected_rev && meta.claimed_revision == op.revision
}

pub(crate) fn save_resume_claim_meta_in_app(
    app: &store::App<'_>,
    op_id: Uuid,
    ctx: &ResumeIdempotencyCtx,
    original_revision: i64,
    claimed_revision: i64,
) -> Result<(), PluginServiceError> {
    let meta = ResumeClaimMeta {
        actor: ctx.actor.to_string(),
        idempotency_key: ctx.idempotency_key.clone(),
        method: ctx.request.method.clone(),
        path: ctx.request.path.clone(),
        body_digest: ctx.request.body_digest,
        original_revision,
        claimed_revision,
    };
    let raw = serde_json::to_string(&meta).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    app.put_setting(&resume_claim_setting_key(op_id), &raw)?;
    Ok(())
}

pub(crate) fn settle_resume_idempotency_in_app(
    app: &store::App<'_>,
    op_id: Uuid,
    ctx: Option<&ResumeIdempotencyCtx>,
) -> Result<Option<OperationAdminView>, PluginServiceError> {
    let setting_key = resume_claim_setting_key(op_id);
    let stored_raw = app.get_setting(&setting_key)?;
    let _ = app.delete_setting(&setting_key)?;
    let resolved = if let Some(c) = ctx {
        Some((c.actor, c.idempotency_key.clone(), c.request.clone()))
    } else if let Some(raw) = stored_raw
        && let Ok(meta) = serde_json::from_str::<ResumeClaimMeta>(&raw)
        && let Ok(actor) = meta.actor.parse::<domain::UserId>()
    {
        Some((
            actor,
            meta.idempotency_key,
            CanonicalPluginRequest {
                method: meta.method,
                path: meta.path,
                body_digest: meta.body_digest,
            },
        ))
    } else {
        None
    };
    let Some((actor, idem_key, req)) = resolved else {
        return Ok(None);
    };
    let view = load_app_operation_admin_view(app, op_id)?;
    let json = serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    app.put_plugin_idempotency_envelope(
        &PluginSubject::User(actor),
        "operation-resume",
        &idem_key,
        &req,
        &json,
    )?;
    Ok(Some(view))
}
