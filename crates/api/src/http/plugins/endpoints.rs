use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use domain::UserId;
use plugin_api::{EndpointRequest, EndpointResponse, ErrorCode, Failure, Header};
use store::plugins::{PluginCommandCause, PluginLifecycle, PluginSubject};
use uuid::Uuid;

use super::service_error_response;
use crate::management::ApiState;
use crate::plugins::PluginServiceError;

pub(super) const MAX_ENDPOINT_BODY_BYTES: usize = 1024 * 1024;

pub(super) async fn invoke_plugin_endpoint(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    method: Method,
    headers: HeaderMap,
    Path((id, endpoint_path)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
    body: Bytes,
) -> Response {
    if !matches!(
        method,
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return crate::http::err(
            StatusCode::METHOD_NOT_ALLOWED,
            "request.method_not_allowed",
            "unsupported endpoint HTTP method",
        );
    }
    if body.len() > MAX_ENDPOINT_BODY_BYTES {
        return service_error_response(PluginServiceError::PayloadTooLarge {
            code: "plugin.payload_too_large",
            message: "endpoint request body exceeds 1 MiB limit".into(),
        });
    }
    if crate::plugins::has_attribution_headers(&headers)
        && let Err(err) = state
            .plugins()
            .verify_plugin_attribution(&state, &id, &headers)
            .await
    {
        return service_error_response(err);
    }
    let normalized_path = match normalize_endpoint_path(&endpoint_path) {
        Ok(p) => p,
        Err(err) => return service_error_response(err),
    };
    let query = parse_query_headers(raw_query.as_deref());
    let req = EndpointRequest {
        method: method.as_str().to_string(),
        path: normalized_path,
        query,
        body: body.to_vec(),
    };
    let worker = state.clone();
    let run_res = state
        .http_work
        .run_to_completion(move || execute_endpoint_blocking(&worker, &id, user_id, req))
        .await;
    match run_res {
        Ok(Ok(ep_resp)) => build_sanitized_http_response(ep_resp),
        Ok(Err(err)) => service_error_response(err),
        Err(error) => {
            tracing::warn!(%error, "plugin endpoint worker unavailable");
            crate::http::store_wait::busy_response()
        }
    }
}

fn normalize_endpoint_path(raw: &str) -> Result<String, PluginServiceError> {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.chars().any(char::is_control)
        || trimmed
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "invalid plugin endpoint path",
        ));
    }
    Ok(format!("/{trimmed}"))
}

fn parse_query_headers(raw_query: Option<&str>) -> Vec<Header> {
    let Some(q) = raw_query.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    url::form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| Header {
            name: k.into_owned(),
            value: v.into_owned(),
        })
        .collect()
}

fn execute_endpoint_blocking(
    state: &ApiState,
    plugin_id: &str,
    user_id: UserId,
    req: EndpointRequest,
) -> Result<EndpointResponse, PluginServiceError> {
    let plugin = state
        .store
        .get_plugin(plugin_id)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
    if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
        return Err(PluginServiceError::forbidden(
            "plugin.not_enabled",
            "plugin is not enabled",
        ));
    }
    let authority = state.store.issue_plugin_state_authority(&plugin, None)?;
    let cause = PluginCommandCause::root(format!("http-endpoint:{plugin_id}:{}", Uuid::new_v4()));
    state
        .plugins
        .invoke_endpoint(
            state,
            &plugin,
            authority,
            PluginSubject::User(user_id),
            cause,
            req,
        )
        .map_err(map_endpoint_failure)
}

fn map_endpoint_failure(err: Failure) -> PluginServiceError {
    match err {
        Failure::Guest(api_err) => match api_err.code {
            ErrorCode::NotFound => {
                PluginServiceError::not_found("plugin.endpoint_not_found", api_err.message)
            }
            ErrorCode::PermissionDenied => {
                PluginServiceError::forbidden("plugin.permission_denied", api_err.message)
            }
            ErrorCode::InvalidArgument => {
                PluginServiceError::invalid("plugin.invalid_input", api_err.message)
            }
            ErrorCode::StaleHandle => {
                PluginServiceError::conflict("plugin.stale_handle", api_err.message)
            }
            ErrorCode::StaleGeneration => {
                PluginServiceError::conflict("plugin.stale_generation", api_err.message)
            }
            ErrorCode::Conflict | ErrorCode::QuotaExceeded => {
                PluginServiceError::conflict("plugin.conflict", api_err.message)
            }
            ErrorCode::WrongContext => PluginServiceError::WrongContext,
            ErrorCode::UnknownOutcome => {
                PluginServiceError::conflict("plugin.unknown_effect", api_err.message)
            }
            ErrorCode::Busy | ErrorCode::DeadlineExceeded => PluginServiceError::Busy {
                code: "plugin.busy",
                message: api_err.message,
            },
            ErrorCode::Unsupported => PluginServiceError::Unprocessable {
                code: "plugin.unsupported",
                message: api_err.message,
            },
            ErrorCode::Cancelled | ErrorCode::Upstream => PluginServiceError::Io(api_err.message),
        },
        Failure::Busy | Failure::DeadlineExceeded => PluginServiceError::Busy {
            code: "plugin.busy",
            message: err.to_string(),
        },
        Failure::InvalidInput(msg) => PluginServiceError::invalid("plugin.invalid_input", msg),
        Failure::Reentrant => PluginServiceError::WrongContext,
        Failure::Unsupported => PluginServiceError::Unprocessable {
            code: "plugin.unsupported",
            message: err.to_string(),
        },
        Failure::Trap(msg) | Failure::InvalidComponent(msg) | Failure::InvalidPatch(msg) => {
            PluginServiceError::Io(msg)
        }
        Failure::Cancelled => PluginServiceError::conflict("plugin.cancelled", "cancelled"),
    }
}

fn build_sanitized_http_response(ep: EndpointResponse) -> Response {
    if ep.body.len() > MAX_ENDPOINT_BODY_BYTES {
        return service_error_response(PluginServiceError::PayloadTooLarge {
            code: "plugin.response_too_large",
            message: "plugin endpoint response exceeds 1 MiB limit".into(),
        });
    }
    let Ok(status) = StatusCode::from_u16(ep.status) else {
        return service_error_response(PluginServiceError::Unprocessable {
            code: "plugin.invalid_endpoint_response",
            message: "invalid HTTP status from plugin endpoint".into(),
        });
    };
    let mut chosen_ct: Option<HeaderValue> = None;
    let mut chosen_loc: Option<HeaderValue> = None;
    for h in &ep.headers {
        let name_lc = h.name.trim().to_ascii_lowercase();
        if name_lc == "set-cookie" {
            return service_error_response(PluginServiceError::Unprocessable {
                code: "plugin.invalid_endpoint_response",
                message: "Set-Cookie header is forbidden on plugin endpoints".into(),
            });
        }
        if name_lc == "location" {
            let v = h.value.trim();
            if !v.starts_with('/') || v.starts_with("//") || v.contains('\\') {
                return service_error_response(PluginServiceError::Unprocessable {
                    code: "plugin.invalid_endpoint_response",
                    message: "external Location header is forbidden on plugin endpoints".into(),
                });
            }
            let Ok(hv) = HeaderValue::from_str(v) else {
                return service_error_response(PluginServiceError::Unprocessable {
                    code: "plugin.invalid_endpoint_response",
                    message: "invalid Location header value from plugin endpoint".into(),
                });
            };
            chosen_loc = Some(hv);
        } else if name_lc == "content-type" {
            let raw_ct = h.value.trim();
            let ct_lc = raw_ct.to_ascii_lowercase();
            let essence = ct_lc.split(';').next().unwrap_or("").trim();
            if !matches!(essence, "application/json" | "text/plain") {
                return service_error_response(PluginServiceError::Unprocessable {
                    code: "plugin.invalid_endpoint_response",
                    message: "unsupported Content-Type from plugin endpoint".into(),
                });
            }
            let Ok(hv) = HeaderValue::from_str(raw_ct) else {
                return service_error_response(PluginServiceError::Unprocessable {
                    code: "plugin.invalid_endpoint_response",
                    message: "invalid Content-Type header value from plugin endpoint".into(),
                });
            };
            chosen_ct = Some(hv);
        }
    }
    let default_ct = if serde_json::from_slice::<serde_json::Value>(&ep.body).is_ok() {
        HeaderValue::from_static("application/json")
    } else {
        HeaderValue::from_static("text/plain; charset=utf-8")
    };
    let mut resp = (status, ep.body).into_response();
    let hdrs = resp.headers_mut();
    hdrs.insert(header::CONTENT_TYPE, chosen_ct.unwrap_or(default_ct));
    hdrs.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    hdrs.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Some(loc) = chosen_loc {
        hdrs.insert(header::LOCATION, loc);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_api::ApiError;

    #[test]
    fn sanitized_response_rejects_backslash_location_jsonp_and_invalid_header_values() {
        let bad_loc = build_sanitized_http_response(EndpointResponse {
            status: 302,
            headers: vec![Header {
                name: "Location".into(),
                value: "/\\evil.example/p".into(),
            }],
            body: Vec::new(),
        });
        assert_eq!(bad_loc.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let ctl_loc = build_sanitized_http_response(EndpointResponse {
            status: 302,
            headers: vec![Header {
                name: "Location".into(),
                value: "/ok\x07bad".into(),
            }],
            body: Vec::new(),
        });
        assert_eq!(ctl_loc.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let jsonp = build_sanitized_http_response(EndpointResponse {
            status: 200,
            headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/jsonp".into(),
            }],
            body: b"cb({})".to_vec(),
        });
        assert_eq!(jsonp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let ctl_ct = build_sanitized_http_response(EndpointResponse {
            status: 200,
            headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/json\x07".into(),
            }],
            body: b"{}".to_vec(),
        });
        assert_eq!(ctl_ct.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let ok_txt = build_sanitized_http_response(EndpointResponse {
            status: 200,
            headers: vec![Header {
                name: "Content-Type".into(),
                value: "text/plain; charset=utf-8".into(),
            }],
            body: b"hi".to_vec(),
        });
        assert_eq!(ok_txt.status(), StatusCode::OK);
    }

    #[test]
    fn map_endpoint_failure_preserves_wrong_context_and_unknown_outcome() {
        let wc = map_endpoint_failure(Failure::Guest(ApiError {
            code: ErrorCode::WrongContext,
            message: "forbidden in GET".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        }));
        assert!(matches!(wc, PluginServiceError::WrongContext));

        let unk = map_endpoint_failure(Failure::Guest(ApiError {
            code: ErrorCode::UnknownOutcome,
            message: "ambiguous".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        }));
        assert!(matches!(
            unk,
            PluginServiceError::Conflict {
                code: "plugin.unknown_effect",
                ..
            }
        ));
    }
}
