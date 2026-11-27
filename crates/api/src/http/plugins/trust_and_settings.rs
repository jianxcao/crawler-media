use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use domain::UserId;

use super::dto::{
    CreateTrustKeyRequest, EmptyObjectRequest, EnableDeveloperModeRequest, PrepareTrustKeyRequest,
    ReauthRequest, RevokeTrustKeyRequest,
};
use super::extractors::{CanonicalWrite, StrictPluginJson, parse_pagination_only};
use super::{
    accepted_op, created_with_etag_location, ok_json, ok_paged, ok_with_etag,
    service_error_response,
};
use crate::management::ApiState;
use crate::plugins::{
    CreateTrustKeyInput, DisableDeveloperModeInput, EnableDeveloperModeInput, PrepareTrustKeyInput,
    RevokeTrustKeyInput,
};

pub(super) async fn plugin_reauth(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    StrictPluginJson(body): StrictPluginJson<ReauthRequest>,
) -> Response {
    match state
        .plugins()
        .issue_reauth_token(&state, user_id, body.password, body.purpose)
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn list_trust_keys(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let (cursor, limit) = match parse_pagination_only(raw_query.as_deref()) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match state.plugins().list_trust_keys(&state, cursor, limit).await {
        Ok(page) => ok_paged(page),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn prepare_trust_key(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    StrictPluginJson(body): StrictPluginJson<PrepareTrustKeyRequest>,
) -> Response {
    match state
        .plugins()
        .prepare_trust_key(
            &state,
            user_id,
            PrepareTrustKeyInput {
                public_key: body.public_key,
                label: body.label,
            },
        )
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn create_trust_key(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    w: CanonicalWrite<CreateTrustKeyRequest>,
) -> Response {
    match state
        .plugins()
        .create_trust_key(
            &state,
            user_id,
            CreateTrustKeyInput {
                label: w.body.label,
                public_key: w.body.public_key,
                fingerprint: w.body.fingerprint,
                consent_token: w.body.consent_token,
                reauth_token: w.body.reauth_token,
                expected_revision: w.expected_revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(view) => created_with_etag_location(view.revision, "/api/v1/plugin-trust-keys", &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn revoke_trust_key(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(key_id): Path<String>,
    w: CanonicalWrite<RevokeTrustKeyRequest>,
) -> Response {
    match state
        .plugins()
        .revoke_trust_key(
            &state,
            user_id,
            RevokeTrustKeyInput {
                key_id,
                reason: w.body.reason,
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

pub(super) async fn get_settings(State(state): State<ApiState>) -> Response {
    match state.plugins().get_plugin_settings(&state).await {
        Ok(view) => ok_with_etag(StatusCode::OK, view.revision, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn enable_developer_mode(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    w: CanonicalWrite<EnableDeveloperModeRequest>,
) -> Response {
    match state
        .plugins()
        .enable_developer_mode(
            &state,
            user_id,
            EnableDeveloperModeInput {
                reauth_token: w.body.reauth_token,
                expected_revision: w.expected_revision,
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

pub(super) async fn disable_developer_mode(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    w: CanonicalWrite<EmptyObjectRequest>,
) -> Response {
    let _ = w.body;
    match state
        .plugins()
        .disable_developer_mode(
            &state,
            user_id,
            DisableDeveloperModeInput {
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
