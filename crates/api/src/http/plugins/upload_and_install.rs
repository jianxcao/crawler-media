use axum::Extension;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use domain::UserId;
use sha2::{Digest, Sha256};
use store::plugins::{CanonicalPluginRequest, PluginLifecycle};
use uuid::Uuid;

use super::dto::{ExecuteInstallRequest, PrepareInstallRequest};
use super::extractors::{
    CanonicalWrite, extract_upload_body, parse_idempotency_key, parse_if_match, parse_lifecycle,
    parse_query_pairs, validate_cursor, validate_limit,
};
use super::multipart::parse_upload_multipart;
use super::{accepted_op, ok_json, ok_paged, ok_with_etag, service_error_response};
use crate::http::err;
use crate::management::ApiState;
use crate::plugins::{ExecuteInstallInput, PrepareInstallInput, StageUploadInput};

pub(super) async fn stage_upload(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    req: Request,
) -> Response {
    let headers = req.headers().clone();
    let method = req.method().as_str().to_ascii_uppercase();
    let raw_path = req.uri().path();
    let path = raw_path
        .strip_prefix("/api/v1")
        .unwrap_or(raw_path)
        .to_string();

    let idempotency_key = match parse_idempotency_key(&headers) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let expected_rev = match parse_if_match(&headers) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if expected_rev != 0 {
        return err(
            StatusCode::CONFLICT,
            "plugin.revision_conflict",
            "upload requires If-Match: \"0\"",
        );
    }

    let body_bytes = match extract_upload_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let parsed = match parse_upload_multipart(&headers, &body_bytes) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let upload_id = match parsed.upload_id.as_deref() {
        Some(raw) => match Uuid::parse_str(raw.trim()) {
            Ok(id) => id,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    "upload_id must be a valid UUID",
                );
            }
        },
        None => {
            return err(
                StatusCode::BAD_REQUEST,
                "plugin.invalid_input",
                "multipart field 'upload_id' is required",
            );
        }
    };

    let expected_sha256 = match resolve_expected_sha256(&headers, parsed.expected_sha256) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let reauth_token = match resolve_upload_reauth(&headers, parsed.reauth_token) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let digest = canonical_upload_digest(
        upload_id,
        &parsed.file,
        parsed.signature.as_deref(),
        expected_sha256.as_deref(),
        &reauth_token,
    );

    let input = StageUploadInput {
        actor_user_id: user_id,
        upload_id,
        cmplugin_bytes: parsed.file,
        signature_text: parsed.signature,
        expected_sha256,
        reauth_token,
        idempotency_key,
        request: CanonicalPluginRequest {
            method,
            path,
            body_digest: digest,
        },
    };
    match state.plugins().stage_upload(&state, input).await {
        Ok(view) => {
            let mut resp = ok_json(StatusCode::CREATED, &view);
            let loc = format!("/api/v1/plugin-uploads/{}", view.upload_id);
            if let Ok(val) = HeaderValue::from_str(&loc) {
                resp.headers_mut().insert(header::LOCATION, val);
            }
            resp
        }
        Err(err) => service_error_response(err),
    }
}

fn normalize_sha256_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("sha256:") {
        rest.trim().to_ascii_lowercase()
    } else if let Some(rest) = trimmed.strip_prefix("SHA256:") {
        rest.trim().to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn resolve_expected_sha256(
    headers: &HeaderMap,
    field: Option<String>,
) -> Result<Option<String>, Response> {
    let from_hdr = match headers.get("x-plugin-sha256") {
        Some(v) => Some(normalize_sha256_str(v.to_str().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "plugin.invalid_input",
                "invalid X-Plugin-Sha256 header",
            )
        })?)),
        None => None,
    };
    let from_field = field.map(|s| normalize_sha256_str(&s));
    match (from_hdr, from_field) {
        (Some(h), Some(f)) if h != f => Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "X-Plugin-Sha256 header conflicts with expected_sha256 field",
        )),
        (Some(h), _) | (None, Some(h)) => {
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    "expected_sha256 must be 64 hex characters",
                ));
            }
            Ok(Some(h))
        }
        (None, None) => Ok(None),
    }
}

fn resolve_upload_reauth(headers: &HeaderMap, field: Option<String>) -> Result<String, Response> {
    let from_hdr = match headers.get("x-plugin-reauth") {
        Some(v) => Some(
            v.to_str()
                .map_err(|_| {
                    err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_input",
                        "invalid X-Plugin-Reauth header",
                    )
                })?
                .to_string(),
        ),
        None => None,
    };
    let tok = match (field, from_hdr) {
        (Some(f), Some(h)) if f != h => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "plugin.invalid_input",
                "X-Plugin-Reauth header conflicts with reauth_token field",
            ));
        }
        (Some(f), _) | (None, Some(f)) => f,
        (None, None) => {
            return Err(err(
                StatusCode::FORBIDDEN,
                "plugin.reauth_required",
                "reauth_token is required",
            ));
        }
    };
    if tok.is_empty() {
        return Err(err(
            StatusCode::FORBIDDEN,
            "plugin.reauth_required",
            "reauth_token is required",
        ));
    }
    Ok(tok)
}

fn canonical_upload_digest(
    upload_id: Uuid,
    file_bytes: &[u8],
    sig: Option<&str>,
    expected_sha256: Option<&str>,
    reauth_token: &str,
) -> [u8; 32] {
    let file_hash: [u8; 32] = Sha256::digest(file_bytes).into();
    let mut h = Sha256::new();
    h.update(upload_id.as_bytes());
    h.update(file_hash);
    update_opt_str(&mut h, sig);
    update_opt_str(&mut h, expected_sha256);
    h.update((reauth_token.len() as u64).to_le_bytes());
    h.update(reauth_token.as_bytes());
    h.finalize().into()
}

fn update_opt_str(h: &mut Sha256, val: Option<&str>) {
    match val {
        Some(s) => {
            h.update([1]);
            h.update((s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
        None => h.update([0]),
    }
}

pub(super) async fn prepare_install(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    w: CanonicalWrite<PrepareInstallRequest>,
) -> Response {
    if w.expected_revision != 0 {
        return err(
            StatusCode::CONFLICT,
            "plugin.revision_conflict",
            "prepare install requires If-Match: \"0\"",
        );
    }
    match state
        .plugins()
        .prepare_install(
            &state,
            PrepareInstallInput {
                actor_user_id: user_id,
                upload_id: w.body.upload_id,
                repo_id: w.body.repo_id,
                plugin_id: w.body.plugin_id,
                version: w.body.version,
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

pub(super) async fn execute_install(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    w: CanonicalWrite<ExecuteInstallRequest>,
) -> Response {
    match state
        .plugins()
        .execute_install(
            &state,
            ExecuteInstallInput {
                actor_user_id: user_id,
                preparation_id: w.body.preparation_id,
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

fn is_valid_plugin_id_cursor(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && raw.contains('.')
        && !raw.starts_with('.')
        && !raw.ends_with('.')
        && raw.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

pub(super) async fn list_plugins(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let mut lifecycle: Option<PluginLifecycle> = None;
    let mut cursor: Option<String> = None;
    let mut limit: Option<usize> = None;
    for (k, v) in parse_query_pairs(raw_query.as_deref()) {
        match k.as_str() {
            "state" => match parse_lifecycle(&v) {
                Ok(l) => lifecycle = Some(l),
                Err(resp) => return resp,
            },
            "cursor" => match validate_cursor(v) {
                Ok(c) if is_valid_plugin_id_cursor(&c) => cursor = Some(c),
                Ok(_) => {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_input",
                        "cursor must be a valid plugin id",
                    );
                }
                Err(resp) => return resp,
            },
            "limit" => match validate_limit(&v) {
                Ok(l) => limit = Some(l),
                Err(resp) => return resp,
            },
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                );
            }
        }
    }
    match state
        .plugins()
        .list_plugins(&state, lifecycle, cursor, limit)
        .await
    {
        Ok(page) => ok_paged(page),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn get_plugin(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    match state.plugins().get_plugin(&state, &id).await {
        Ok(view) => ok_with_etag(StatusCode::OK, view.revision, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn dev_sideload(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    req: Request,
) -> Response {
    let headers = req.headers().clone();
    let body_bytes = match extract_upload_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let is_multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/form-data"));
    let cmplugin_bytes = if is_multipart {
        match parse_upload_multipart(&headers, &body_bytes) {
            Ok(parsed) => parsed.file,
            Err(resp) => return resp,
        }
    } else {
        body_bytes.to_vec()
    };

    match state
        .plugins()
        .sideload_dev_package(&state, user_id, cmplugin_bytes)
        .await
    {
        Ok((created, view)) => {
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            ok_with_etag(status, view.revision, &view)
        }
        Err(err) => service_error_response(err),
    }
}
