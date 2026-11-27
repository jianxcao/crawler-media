use axum::Extension;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::UserId;

use super::dto::{
    GrantPermissionsRequest, PreparePermissionsRequest, RevokePermissionsRequest,
    UpdateConfigRequest, ValidateConfigRequest,
};
use super::extractors::{CanonicalWrite, StrictPluginJson};
use super::{accepted_op, ok_json, ok_with_etag, service_error_response};
use crate::management::ApiState;
use crate::plugins::{
    GrantPermissionsInput, PreparePermissionsInput, RevokePermissionsInput, UpdateConfigInput,
    ValidateConfigInput,
};

pub(super) async fn get_config_schema(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.plugins().get_config_schema(&state, &id).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn get_config(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    match state.plugins().get_config(&state, &id).await {
        Ok(view) => ok_with_etag(StatusCode::OK, view.revision, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn validate_config(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    StrictPluginJson(body): StrictPluginJson<ValidateConfigRequest>,
) -> Response {
    match state
        .plugins()
        .validate_config(
            &state,
            &id,
            ValidateConfigInput {
                schema_revision: body.schema_revision,
                values: body.values,
                secrets: body.secrets,
            },
        )
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn update_config(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path(id): Path<String>,
    w: CanonicalWrite<UpdateConfigRequest>,
) -> Response {
    if let Err(err) = state
        .plugins()
        .verify_config_attribution(&state, &id, &headers)
        .await
    {
        return service_error_response(err);
    }
    match state
        .plugins()
        .update_config(
            &state,
            &id,
            UpdateConfigInput {
                actor_user_id: user_id,
                schema_revision: w.body.schema_revision,
                values: w.body.values,
                secrets: w.body.secrets,
                expected_revision: w.expected_revision,
                site_id: w.body.site_id,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(view) => ok_with_etag(StatusCode::OK, view.revision, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn get_permissions(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.plugins().get_permissions(&state, &id).await {
        Ok(view) => ok_with_etag(StatusCode::OK, view.revision, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn prepare_permissions(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    StrictPluginJson(body): StrictPluginJson<PreparePermissionsRequest>,
) -> Response {
    match state
        .plugins()
        .prepare_permissions(
            &state,
            user_id,
            &id,
            PreparePermissionsInput {
                requested_grants: body.requested_grants,
            },
        )
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn grant_permissions(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<GrantPermissionsRequest>,
) -> Response {
    match state
        .plugins()
        .grant_permissions(
            &state,
            &id,
            GrantPermissionsInput {
                actor_user_id: user_id,
                consent_token: w.body.consent_token,
                grants: w.body.grants,
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

pub(super) async fn revoke_permissions(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<RevokePermissionsRequest>,
) -> Response {
    match state
        .plugins()
        .revoke_permissions(
            &state,
            &id,
            RevokePermissionsInput {
                actor_user_id: user_id,
                grants: w.body.grants,
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
