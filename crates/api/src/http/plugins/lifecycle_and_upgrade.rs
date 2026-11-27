use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use domain::UserId;

use super::dto::{
    DisablePluginRequest, EmptyObjectRequest, ExecuteStateResetRequest, ExecuteUpgradeRequest,
    PrepareStateResetRequest, PrepareUpgradeRequest, RollbackRequest, UninstallPluginRequest,
};
use super::extractors::{CanonicalWrite, StrictPluginJson, resolve_security_scopes};
use super::{accepted_op, ok_json, service_error_response};
use crate::management::ApiState;
use crate::plugins::{
    DisablePluginInput, EnablePluginInput, ExecuteStateResetInput, ExecuteUpgradeInput,
    PrepareStateResetInput, PrepareUpgradeInput, RollbackInput, UninstallPluginInput,
    UpgradeTargetConfigInput,
};

pub(super) async fn enable_plugin(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<EmptyObjectRequest>,
) -> Response {
    let _ = w.body;
    match state
        .plugins()
        .enable_plugin(
            &state,
            &id,
            EnablePluginInput {
                actor_user_id: user_id,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn disable_plugin(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<DisablePluginRequest>,
) -> Response {
    match state
        .plugins()
        .disable_plugin(
            &state,
            &id,
            DisablePluginInput {
                actor_user_id: user_id,
                reason: w.body.reason,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn uninstall_plugin(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<UninstallPluginRequest>,
) -> Response {
    match state
        .plugins()
        .uninstall_plugin(
            &state,
            &id,
            UninstallPluginInput {
                actor_user_id: user_id,
                remove_private_data: w.body.remove_private_data,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn prepare_upgrade(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<PrepareUpgradeRequest>,
) -> Response {
    let reset_scopes = match w.body.reset_scopes {
        Some(scopes) => match resolve_security_scopes(&state, scopes).await {
            Ok(resolved) => Some(resolved),
            Err(resp) => return resp,
        },
        None => None,
    };
    match state
        .plugins()
        .prepare_upgrade(
            &state,
            &id,
            PrepareUpgradeInput {
                actor_user_id: user_id,
                version: w.body.version,
                upload_id: w.body.upload_id,
                package_hash: w.body.package_hash,
                repo_id: w.body.repo_id,
                target_config: UpgradeTargetConfigInput {
                    schema_revision: w.body.target_config.schema_revision,
                    values: w.body.target_config.values,
                    secrets: w.body.target_config.secrets,
                },
                site_id: w.body.site_id,
                state_policy: w.body.state_policy,
                reset_scopes,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn execute_upgrade(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<ExecuteUpgradeRequest>,
) -> Response {
    match state
        .plugins()
        .execute_upgrade(
            &state,
            &id,
            ExecuteUpgradeInput {
                actor_user_id: user_id,
                preparation_id: w.body.preparation_id,
                consent_token: w.body.consent_token,
                grants: w.body.grants,
                state_policy: w.body.state_policy,
                target_config_digest: w.body.target_config_digest,
                reauth_token: w.body.reauth_token,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn rollback_plugin(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<RollbackRequest>,
) -> Response {
    match state
        .plugins()
        .rollback_plugin(
            &state,
            &id,
            RollbackInput {
                actor_user_id: user_id,
                package_hash: w.body.package_hash,
                preparation_id: w.body.preparation_id,
                consent_token: w.body.consent_token,
                grants: w.body.grants,
                state_policy: w.body.state_policy,
                target_config_digest: w.body.target_config_digest,
                reauth_token: w.body.reauth_token,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn prepare_state_reset(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    StrictPluginJson(body): StrictPluginJson<PrepareStateResetRequest>,
) -> Response {
    let security_scopes = match resolve_security_scopes(&state, body.security_scopes).await {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };
    match state
        .plugins()
        .prepare_state_reset(
            &state,
            user_id,
            &id,
            PrepareStateResetInput { security_scopes },
        )
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn execute_state_reset(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<ExecuteStateResetRequest>,
) -> Response {
    let security_scopes = match resolve_security_scopes(&state, w.body.security_scopes).await {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };
    match state
        .plugins()
        .execute_state_reset(
            &state,
            &id,
            ExecuteStateResetInput {
                actor_user_id: user_id,
                security_scopes,
                consent_token: w.body.consent_token,
                reauth_token: w.body.reauth_token,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}
