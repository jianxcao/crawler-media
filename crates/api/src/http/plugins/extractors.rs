use std::collections::BTreeMap;

use axum::body::{Bytes, to_bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use store::plugins::{
    CanonicalPluginRequest, PluginCommandTarget, PluginLifecycle, PluginSecurityScope,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::dto::{
    GlobalScopeTag, HttpCommandTarget, HttpSecurityScope, SiteObjScope, TaggedSiteScope,
};
use super::multipart::MAX_UPLOAD_BODY_BYTES;
use super::service_error_response;
use crate::http::err;
use crate::http::store_wait::store_read;
use crate::management::ApiState;
use crate::plugins::PluginServiceError;

pub(crate) struct CanonicalWrite<T> {
    pub body: T,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

pub(crate) struct CanonicalOptionalRevisionWrite<T> {
    pub body: T,
    pub expected_revision: Option<i64>,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
}

async fn extract_canonical_body<T, S>(
    req: Request,
    state: &S,
) -> Result<(T, String, String, [u8; 32]), Response>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    let method = req.method().as_str().to_ascii_uppercase();
    let raw_path = req.uri().path();
    let path = raw_path
        .strip_prefix("/api/v1")
        .unwrap_or(raw_path)
        .to_string();
    let bytes = Bytes::from_request(req, state)
        .await
        .map_err(map_bytes_rejection)?;
    let body: T = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    let canonical = canonical_json_bytes(&value).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            "request.invalid",
            &format!("invalid JSON body: {e}"),
        )
    })?;
    let body_digest: [u8; 32] = Sha256::digest(&canonical).into();
    Ok((body, method, path, body_digest))
}

impl<T, S> FromRequest<S> for CanonicalWrite<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let headers = req.headers().clone();
        let idempotency_key = parse_idempotency_key(&headers)?;
        let expected_revision = parse_if_match(&headers)?;
        let (body, method, path, body_digest) = extract_canonical_body(req, state).await?;
        Ok(Self {
            body,
            expected_revision,
            idempotency_key,
            request: CanonicalPluginRequest {
                method,
                path,
                body_digest,
            },
        })
    }
}

impl<T, S> FromRequest<S> for CanonicalOptionalRevisionWrite<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let headers = req.headers().clone();
        let idempotency_key = parse_idempotency_key(&headers)?;
        let expected_revision = if headers.contains_key(axum::http::header::IF_MATCH) {
            Some(parse_if_match(&headers)?)
        } else {
            None
        };
        let (body, method, path, body_digest) = extract_canonical_body(req, state).await?;
        Ok(Self {
            body,
            expected_revision,
            idempotency_key,
            request: CanonicalPluginRequest {
                method,
                path,
                body_digest,
            },
        })
    }
}

fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = Vec::new();
    write_canonical_json(&mut out, value)?;
    Ok(out)
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

pub(super) struct StrictPluginJson<T>(pub T);

impl<T, S> FromRequest<S> for StrictPluginJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(map_bytes_rejection)?;
        let body: T = serde_json::from_slice(&bytes).map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                "request.invalid",
                &format!("invalid JSON body: {e}"),
            )
        })?;
        Ok(Self(body))
    }
}

pub(super) async fn extract_upload_body(req: Request) -> Result<Bytes, Response> {
    to_bytes(req.into_body(), MAX_UPLOAD_BODY_BYTES)
        .await
        .map_err(|_| {
            err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "plugin.package_too_large",
                "upload exceeds 64 MiB package limit",
            )
        })
}

fn map_bytes_rejection(rejection: BytesRejection) -> Response {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "plugin.package_too_large",
            "request body exceeds allowed limit",
        );
    }
    err(
        StatusCode::BAD_REQUEST,
        "request.invalid",
        &rejection.body_text(),
    )
}

pub(super) fn parse_if_match(headers: &HeaderMap) -> Result<i64, Response> {
    let Some(raw_val) = headers.get(axum::http::header::IF_MATCH) else {
        return Err(err(
            StatusCode::PRECONDITION_REQUIRED,
            "plugin.precondition_required",
            "If-Match header is required",
        ));
    };
    let s = raw_val
        .to_str()
        .map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "plugin.invalid_input",
                "invalid If-Match header",
            )
        })?
        .trim();
    if s.starts_with("W/") || s == "*" {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "If-Match must be a strong quoted integer revision",
        ));
    }
    let Some(inner) = s.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "If-Match must be a quoted integer revision",
        ));
    };
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "If-Match must be a non-negative integer revision",
        ));
    }
    let rev = inner.parse::<i64>().map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "If-Match must be a valid integer revision",
        )
    })?;
    Ok(rev)
}

pub(super) fn parse_idempotency_key(headers: &HeaderMap) -> Result<String, Response> {
    let Some(raw) = headers.get("idempotency-key") else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "Idempotency-Key header is required",
        ));
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
    Ok(key.to_string())
}

pub(super) fn parse_uuid_param(raw: &str, name: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            &format!("invalid {name} UUID"),
        )
    })
}

pub(super) fn parse_query_pairs(raw_query: Option<&str>) -> Vec<(String, String)> {
    match raw_query {
        Some(q) => url::form_urlencoded::parse(q.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        None => Vec::new(),
    }
}

pub(super) fn validate_limit(raw: &str) -> Result<usize, Response> {
    let limit = raw.parse::<usize>().map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "limit must be an integer between 1 and 200",
        )
    })?;
    if limit == 0 || limit > 200 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "limit must be between 1 and 200",
        ));
    }
    Ok(limit)
}

pub(super) fn validate_cursor(raw: String) -> Result<String, Response> {
    if raw.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "cursor must not be empty",
        ));
    }
    Ok(raw)
}

pub(super) fn parse_pagination_only(
    raw_query: Option<&str>,
) -> Result<(Option<String>, Option<usize>), Response> {
    let mut cursor = None;
    let mut limit = None;
    for (k, v) in parse_query_pairs(raw_query) {
        match k.as_str() {
            "cursor" => cursor = Some(validate_cursor(v)?),
            "limit" => limit = Some(validate_limit(&v)?),
            _ => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                ));
            }
        }
    }
    Ok((cursor, limit))
}

pub(super) fn parse_lifecycle(raw: &str) -> Result<PluginLifecycle, Response> {
    match raw {
        "installed-disabled" => Ok(PluginLifecycle::InstalledDisabled),
        "enabling" => Ok(PluginLifecycle::Enabling),
        "enabled" => Ok(PluginLifecycle::Enabled),
        "draining" => Ok(PluginLifecycle::Draining),
        "disabled" => Ok(PluginLifecycle::Disabled),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
            "invalid plugin lifecycle state",
        )),
    }
}

pub(super) fn parse_from_timestamp(raw: &str) -> Result<i64, Response> {
    if let Ok(ts) = raw.parse::<i64>() {
        return Ok(ts);
    }
    OffsetDateTime::parse(raw, &Rfc3339)
        .map(|dt| dt.unix_timestamp())
        .map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "plugin.invalid_input",
                "from must be a Unix timestamp or RFC3339 timestamp",
            )
        })
}

pub(super) async fn resolve_security_scopes(
    state: &ApiState,
    scopes: Vec<HttpSecurityScope>,
) -> Result<Vec<PluginSecurityScope>, Response> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let mut site_ids: Vec<Option<domain::SiteId>> = Vec::with_capacity(scopes.len());
    for sc in scopes {
        match sc {
            HttpSecurityScope::Global(GlobalScopeTag::Global) => site_ids.push(None),
            HttpSecurityScope::SiteObj(SiteObjScope { site_id })
            | HttpSecurityScope::TaggedSite(TaggedSiteScope { site: site_id }) => {
                site_ids.push(Some(site_id))
            }
        }
    }
    let resolved = store_read(state, &[store::Db::App], move |store| {
        let mut out = Vec::with_capacity(site_ids.len());
        for opt_id in site_ids {
            match opt_id {
                None => out.push(PluginSecurityScope::global()),
                Some(id) => {
                    let obs = store.observe_site(id).map_err(PluginServiceError::Store)?;
                    let Some(obs) = obs else {
                        return Err(PluginServiceError::invalid(
                            "plugin.scope_invalid",
                            "referenced site_id does not exist",
                        ));
                    };
                    out.push(PluginSecurityScope::for_site(&obs));
                }
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.response())?
    .map_err(service_error_response)?;
    Ok(resolved)
}

pub(super) fn convert_command_targets(targets: Vec<HttpCommandTarget>) -> Vec<PluginCommandTarget> {
    targets
        .into_iter()
        .map(|t| match t {
            HttpCommandTarget::Global => PluginCommandTarget::Global,
            HttpCommandTarget::Site { id } => PluginCommandTarget::Site(id),
            HttpCommandTarget::Library { id } => PluginCommandTarget::Library(id),
            HttpCommandTarget::Downloader { id } => PluginCommandTarget::Downloader(id),
            HttpCommandTarget::Subscribe { id } => PluginCommandTarget::Subscribe(id),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_if_match_rejects_signed_and_non_digit_revisions() {
        for bad in [
            "\"+1\"", "\"+0\"", "\"-0\"", "\"-1\"", "\"\"", "W/\"1\"", "*",
        ] {
            let mut h = HeaderMap::new();
            h.insert(axum::http::header::IF_MATCH, bad.parse().unwrap());
            assert!(parse_if_match(&h).is_err(), "should reject {bad}");
        }
        for (ok, expected) in [("\"0\"", 0), ("\"1\"", 1), ("\"42\"", 42)] {
            let mut h = HeaderMap::new();
            h.insert(axum::http::header::IF_MATCH, ok.parse().unwrap());
            assert_eq!(parse_if_match(&h).unwrap(), expected);
        }
    }

    #[test]
    fn canonical_json_bytes_sorts_nested_keys_deterministically() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{ "b": { "y": 2, "x": 1 }, "a": [1, {"d": 4, "c": 3}] }"#)
                .unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":[1,{"c":3,"d":4}],"b":{"x":1,"y":2}}"#).unwrap();
        let c: serde_json::Value =
            serde_json::from_str(r#"{"a":[1,{"c":3,"d":5}],"b":{"x":1,"y":2}}"#).unwrap();
        let bytes_a = canonical_json_bytes(&a).unwrap();
        let bytes_b = canonical_json_bytes(&b).unwrap();
        let bytes_c = canonical_json_bytes(&c).unwrap();
        assert_eq!(bytes_a, bytes_b);
        assert_eq!(
            String::from_utf8(bytes_a).unwrap(),
            r#"{"a":[1,{"c":3,"d":4}],"b":{"x":1,"y":2}}"#
        );
        assert_ne!(bytes_b, bytes_c);
    }
}
