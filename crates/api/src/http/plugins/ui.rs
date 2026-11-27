use axum::Extension;
use axum::extract::{OriginalUri, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use domain::UserId;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::extractors::{CanonicalWrite, StrictPluginJson};
use super::{ok_json, ok_with_etag, service_error_response};
use crate::management::ApiState;
use crate::plugins::{GrantUiTrustInput, RevokeUiTrustInput, build_ui_contract_view};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareUiTrustRequest {
    #[serde(default)]
    pub package_hash: Option<String>,
    #[serde(default)]
    pub package_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantUiTrustRequest {
    #[serde(default)]
    pub preparation_id: Option<Uuid>,
    #[serde(default)]
    pub package_hash: Option<String>,
    #[serde(default)]
    pub package_sha256: Option<String>,
    pub consent_token: String,
    pub reauth_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevokeUiTrustRequest {
    #[serde(default)]
    pub package_hash: Option<String>,
    #[serde(default)]
    pub package_sha256: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UiQueryRequest {
    pub name: String,
    #[serde(default = "default_empty_obj")]
    pub input: Value,
}

fn default_empty_obj() -> Value {
    json!({})
}

pub(super) async fn plugin_ui_contract() -> Response {
    let view = build_ui_contract_view();
    ok_json(StatusCode::OK, &view)
}

pub(super) async fn get_plugin_ui(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    match state
        .plugins()
        .get_plugin_ui_descriptor(&state, &id, user_id)
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn list_plugin_ui_navigation(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
) -> Response {
    match state
        .plugins()
        .list_plugin_ui_navigation(&state, user_id)
        .await
    {
        Ok(items) => ok_json(StatusCode::OK, &items),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn prepare_ui_trust(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let trimmed = body.trim_ascii();
    let requested_hash = if trimmed.is_empty() {
        None
    } else {
        match serde_json::from_slice::<PrepareUiTrustRequest>(trimmed) {
            Ok(req) => req.package_hash.or(req.package_sha256),
            Err(e) => {
                return crate::http::err(
                    StatusCode::BAD_REQUEST,
                    "request.invalid",
                    &format!("invalid JSON body: {e}"),
                );
            }
        }
    };
    match state
        .plugins()
        .prepare_ui_trust(&state, user_id, &id, requested_hash.as_deref())
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn grant_ui_trust(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<GrantUiTrustRequest>,
) -> Response {
    let rev_hint = w.expected_revision;
    let package_hash = w.body.package_hash.or(w.body.package_sha256);
    match state
        .plugins()
        .grant_ui_trust(
            &state,
            user_id,
            &id,
            GrantUiTrustInput {
                preparation_id: w.body.preparation_id,
                package_hash,
                consent_token: w.body.consent_token,
                reauth_token: w.body.reauth_token,
                expected_revision: rev_hint,
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

pub(super) async fn revoke_ui_trust(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
    w: CanonicalWrite<RevokeUiTrustRequest>,
) -> Response {
    let _reason = w.body.reason;
    let package_hash = w.body.package_hash.or(w.body.package_sha256);
    match state
        .plugins()
        .revoke_ui_trust(
            &state,
            user_id,
            &id,
            RevokeUiTrustInput {
                package_hash,
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

pub(super) async fn serve_plugin_ui_asset(
    State(state): State<ApiState>,
    OriginalUri(original_uri): OriginalUri,
    Path((id, bundle_hash, asset_path)): Path<(String, String, String)>,
) -> Response {
    match state
        .plugins()
        .resolve_ui_asset(&state, &id, &bundle_hash, original_uri.path(), &asset_path)
        .await
    {
        Ok((bytes, content_type)) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            let headers = resp.headers_mut();
            if let Ok(val) = HeaderValue::from_str(&content_type) {
                headers.insert(header::CONTENT_TYPE, val);
            }
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            resp
        }
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn execute_ui_query(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path(id): Path<String>,
    StrictPluginJson(body): StrictPluginJson<UiQueryRequest>,
) -> Response {
    match state
        .plugins()
        .execute_ui_query(&state, &id, user_id, &headers, &body.name, body.input)
        .await
    {
        Ok(data) => ok_json(StatusCode::OK, &data),
        Err(err) => service_error_response(err),
    }
}
