use crate::provider::PlaybackClientInfo;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use domain::UserId;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::provider::{MediaServerProvider, QueryTiming};

#[derive(Clone)]
pub struct AuthUser {
    pub id: UserId,
    pub token: String,
    pub device_id: Option<String>,
}

/// 手写 `Debug`：`token` 是会话凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?user)` 都会把 Bearer token
/// 打进日志与前端"系统日志"。`device_id` 是客户端标识、不是凭证，保留原样，
/// 但调用方仍应优先只记录 `id`。
impl std::fmt::Debug for AuthUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthUser")
            .field("id", &self.id)
            .field("token", &"<redacted>")
            .field("device_id", &self.device_id)
            .finish()
    }
}

pub async fn authenticate(
    provider: Arc<dyn MediaServerProvider>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(token) = request_token(&request) else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let deadline = request
        .extensions()
        .get::<QueryTiming>()
        .map(|timing| timing.deadline);
    let user_id = provider
        .user_id_by_token(&token, deadline)
        .await
        .map_err(|error| {
            tracing::error!(%error, "Jellyfin token lookup failed");
            error.status_code()
        })?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let device_id = request_device_id(&request);
    if let Some(device_id) = device_id.as_deref() {
        provider
            .bind_playback_credential(user_id, device_id, &token, deadline)
            .await
            .map_err(|error| {
                tracing::error!(%user_id, %error, "Playback credential binding failed");
                error.status_code()
            })?;
    }
    request.extensions_mut().insert(user_id);
    request.extensions_mut().insert(AuthUser {
        id: user_id,
        token,
        device_id,
    });
    Ok(next.run(request).await)
}

fn request_device_id(request: &Request) -> Option<String> {
    protocol_value(request.headers(), "X-Emby-Device-Id", "DeviceId").or_else(|| {
        url::form_urlencoded::parse(request.uri().query()?.as_bytes()).find_map(|(key, value)| {
            let trimmed = value.trim();
            ((key.eq_ignore_ascii_case("DeviceId") || key.eq_ignore_ascii_case("device_id"))
                && !trimmed.is_empty())
            .then(|| trimmed.to_string())
        })
    })
}

fn protocol_value(headers: &HeaderMap, header_name: &str, key_name: &str) -> Option<String> {
    let clean = |value: &str| {
        let value = value.trim().trim_matches('"');
        (!value.is_empty()).then(|| value.to_string())
    };
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(clean)
        .or_else(|| {
            ["authorization", "x-emby-authorization"]
                .into_iter()
                .find_map(|header| {
                    let value = headers.get(header)?.to_str().ok()?;
                    let (scheme, credentials) = value.split_once(' ')?;
                    if !scheme.eq_ignore_ascii_case("MediaBrowser")
                        && !scheme.eq_ignore_ascii_case("Emby")
                    {
                        return None;
                    }
                    credentials.split(',').find_map(|part| {
                        let (key, value) = part.trim().split_once('=')?;
                        if key.trim().eq_ignore_ascii_case(key_name) {
                            clean(value)
                        } else {
                            None
                        }
                    })
                })
        })
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ClientIdentity {
    pub(crate) client: Option<String>,
    pub(crate) device_name: Option<String>,
    pub(crate) device_id: Option<String>,
    pub(crate) version: Option<String>,
}

pub(crate) fn extract_client_headers(headers: &HeaderMap) -> ClientIdentity {
    ClientIdentity {
        client: protocol_value(headers, "X-Emby-Client", "Client"),
        device_name: protocol_value(headers, "X-Emby-Device-Name", "Device"),
        device_id: protocol_value(headers, "X-Emby-Device-Id", "DeviceId"),
        version: protocol_value(headers, "X-Emby-Client-Version", "Version"),
    }
}

impl AuthUser {
    pub(crate) fn playback_client(&self, headers: &HeaderMap) -> PlaybackClientInfo {
        let extracted = extract_client_headers(headers);
        let client = extracted.client;
        let device_name = extracted.device_name;
        let client_version = extracted.version;
        let device_id = self.device_id.clone().or(extracted.device_id).unwrap_or_else(|| {
            // No stable protocol identity: correlate this credential/metadata, but
            // never advertise it as a device credential that can be revoked.
            let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
            (&self.token, &client, &device_name, &client_version,
                headers.get(header::USER_AGENT)).hash(&mut fingerprint);
            let fallback = format!("unidentified:{:016x}", fingerprint.finish());
            tracing::warn!(user_id = %self.id, device_id = fallback, "Jellyfin client did not provide DeviceId");
            fallback
        });
        PlaybackClientInfo {
            device_id,
            client,
            device_name,
            client_version,
        }
    }
}

pub async fn authenticate_optional(
    provider: Arc<dyn MediaServerProvider>,
    mut request: Request,
    next: Next,
) -> Response {
    let user_id = if let Some(token) = request_token(&request) {
        match provider
            .user_id_by_token(
                &token,
                request
                    .extensions()
                    .get::<QueryTiming>()
                    .map(|timing| timing.deadline),
            )
            .await
        {
            Ok(user_id) => user_id,
            Err(error) => {
                tracing::warn!(%error, "optional media image authentication lookup failed");
                if request.extensions().get::<QueryTiming>().is_some()
                    && matches!(error, crate::provider::QueryError::Unavailable)
                {
                    return error.status_code().into_response();
                }
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
        url::form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
            (key.eq_ignore_ascii_case("api_key")
                || key.eq_ignore_ascii_case("apikey")
                || key.eq_ignore_ascii_case("token"))
            .then(|| value.into_owned())
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

#[cfg(test)]
mod debug_tests {
    use super::AuthUser;
    use domain::UserId;

    #[test]
    fn debug_never_prints_the_session_token() {
        let user = AuthUser {
            id: UserId::new(),
            token: "session-token-secret".into(),
            device_id: Some("device-1".into()),
        };
        let rendered = format!("{user:?}");
        assert!(
            !rendered.contains("session-token-secret"),
            "Debug 不得输出 token: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("device-1"));
    }
}
