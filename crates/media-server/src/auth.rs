use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use domain::UserId;
use std::sync::Arc;

use crate::provider::MediaServerProvider;

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: UserId,
    pub token: String,
    pub device_id: Option<String>,
}

pub async fn authenticate(
    provider: Arc<dyn MediaServerProvider>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(token) = request_token(&request) else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let user_id = provider
        .user_id_by_token(&token)
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let device_id = request_device_id(&request);
    request.extensions_mut().insert(user_id);
    request
        .extensions_mut()
        .insert(AuthUser { id: user_id, token, device_id });
    Ok(next.run(request).await)
}

fn request_device_id(request: &Request) -> Option<String> {
    if let Some(val) = request.headers().get("X-Emby-Device-Id") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(val) = request
        .headers()
        .get(header::AUTHORIZATION)
        .or_else(|| request.headers().get("x-emby-authorization"))
        .and_then(|v| v.to_str().ok())
    {
        if let Some(device) = device_from_authorization(val) {
            return Some(device.to_string());
        }
    }
    if let Some(query) = request.uri().query() {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k.eq_ignore_ascii_case("DeviceId") || k.eq_ignore_ascii_case("device_id") {
                    let trimmed = v.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }
    None
}

fn device_from_authorization(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("MediaBrowser") && !scheme.eq_ignore_ascii_case("Emby") {
        return None;
    }
    credentials.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("DeviceId")
            .then(|| value.trim().trim_matches('"'))
    })
}

pub async fn authenticate_optional(
    provider: Arc<dyn MediaServerProvider>,
    mut request: Request,
    next: Next,
) -> Response {
    let user_id = if let Some(token) = request_token(&request) {
        match provider.user_id_by_token(&token).await {
            Ok(user_id) => user_id,
            Err(error) => {
                tracing::warn!(%error, "optional media image authentication lookup failed");
                None
            }
        }
    } else {
        None
    };
    request.extensions_mut().insert(user_id);
    next.run(request).await
}

fn request_token(request: &Request) -> Option<String> {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .or_else(|| request.headers().get("x-emby-authorization"))
        .and_then(|value| value.to_str().ok())
        .and_then(token_from_authorization);
    let client_header = ["x-emby-token", "x-mediabrowser-token"]
        .into_iter()
        .find_map(|name| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
        });
    let query_token = request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.eq_ignore_ascii_case("api_key")
                || key.eq_ignore_ascii_case("apikey")
                || key.eq_ignore_ascii_case("token"))
            .then(|| value.to_string())
        })
    });
    authorization
        .map(str::to_string)
        .or_else(|| client_header.map(str::to_string))
        .or(query_token)
}

fn token_from_authorization(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        return Some(credentials.trim());
    }
    if !scheme.eq_ignore_ascii_case("MediaBrowser") && !scheme.eq_ignore_ascii_case("Emby") {
        return None;
    }
    credentials.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("Token")
            .then(|| value.trim().trim_matches('"'))
    })
}
