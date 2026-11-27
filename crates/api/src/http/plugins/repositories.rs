use std::collections::BTreeMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::UserId;
use sha2::{Digest, Sha256};
use store::plugins::CanonicalPluginRequest;

use super::dto::SavePluginRepositoriesRequest;
use super::{ok_json, service_error_response};
use crate::http::err;
use crate::management::ApiState;
use crate::plugins::{PluginRepositoryInput, SavePluginRepositoriesInput};

pub(super) async fn list_repositories(State(state): State<ApiState>) -> Response {
    match state.plugins().list_repositories(&state).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

pub(super) async fn save_repositories(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    req: Request,
) -> Response {
    let input = match extract_save_repositories_input(&state, user_id, req).await {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    match state.plugins().save_repositories(&state, input).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

async fn extract_save_repositories_input(
    state: &ApiState,
    actor_user_id: UserId,
    req: Request,
) -> Result<SavePluginRepositoriesInput, Response> {
    let headers = req.headers().clone();
    let method = req.method().as_str().to_ascii_uppercase();
    let raw_path = req.uri().path();
    let path = raw_path
        .strip_prefix("/api/v1")
        .unwrap_or(raw_path)
        .to_string();

    let idempotency_key = parse_optional_idempotency_key(&headers)?;
    let bytes = Bytes::from_request(req, state).await.map_err(|rejection| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &rejection.body_text(),
        )
    })?;
    let body: SavePluginRepositoriesRequest = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    let request = match idempotency_key {
        Some(_) => Some(compute_canonical_request(&bytes, method, path)?),
        None => None,
    };
    let repositories = body
        .repositories
        .into_iter()
        .map(|r| PluginRepositoryInput {
            id: r.id,
            name: r.name,
            index_url: r.index_url,
            enabled: r.enabled,
            builtin: r.builtin,
        })
        .collect();
    Ok(SavePluginRepositoriesInput {
        actor_user_id,
        repositories,
        idempotency_key,
        request,
    })
}

pub(super) async fn get_catalog(
    State(state): State<ApiState>,
    Path(repo_id): Path<String>,
) -> Response {
    match state
        .plugins()
        .get_repository_catalog(&state, &repo_id)
        .await
    {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(err) => service_error_response(err),
    }
}

fn parse_optional_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, Response> {
    let Some(raw) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = raw.to_str().map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "Idempotency-Key must be valid UTF-8",
        )
    })?;
    if key.is_empty() || key.len() > 256 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "Idempotency-Key must be 1..=256 bytes",
        ));
    }
    Ok(Some(key.to_string()))
}

fn compute_canonical_request(
    bytes: &[u8],
    method: String,
    path: String,
) -> Result<CanonicalPluginRequest, Response> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    let mut out = Vec::new();
    write_canonical_json(&mut out, &value).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    Ok(CanonicalPluginRequest {
        method,
        path,
        body_digest: Sha256::digest(&out).into(),
    })
}

fn write_canonical_json(
    out: &mut Vec<u8>,
    value: &serde_json::Value,
) -> Result<(), serde_json::Error> {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(n) => serde_json::to_writer(&mut *out, n)?,
        serde_json::Value::String(s) => serde_json::to_writer(&mut *out, s)?,
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical_json(out, item)?;
            }
            out.push(b']');
        }
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<&str, &serde_json::Value> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            out.push(b'{');
            for (idx, (k, v)) in sorted.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, k)?;
                out.push(b':');
                write_canonical_json(out, v)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}
