//! Jellyfin/Emby system, authentication and device endpoints.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};

use crate::dto::user_profile_json;
use crate::provider::AuthenticateError;

use super::AppState;

const JELLYFIN_PROTOCOL_VERSION: &str = "10.9.11";

#[derive(Deserialize)]
pub struct AuthBody {
    #[serde(alias = "Username")]
    pub username: String,
    #[serde(default, alias = "Pw", alias = "Password", alias = "password")]
    pub pw: String,
}

fn system_info_payload(server_id: &str) -> Value {
    json!({
        "Id": server_id,
        "ServerName": "crawler-media",
        "ProductName": "crawler-media",
        "Version": JELLYFIN_PROTOCOL_VERSION,
        "StartupWizardCompleted": true,
        "OperatingSystem": std::env::consts::OS,
        "HasPendingRestart": false,
        "IsShuttingDown": false,
        "SupportsLibraryMonitor": true,
        "CanLaunchWebBrowser": false,
    })
}

pub(super) async fn public_system_info(State(state): State<AppState>) -> Json<Value> {
    Json(system_info_payload(&state.provider.server_id()))
}

pub(super) async fn system_info(State(state): State<AppState>) -> Json<Value> {
    Json(system_info_payload(&state.provider.server_id()))
}

pub(super) async fn system_endpoint() -> Json<Value> {
    Json(json!({
        "IsLocal": true,
        "IsInNetwork": true,
    }))
}

pub(super) async fn sessions_capabilities() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub(super) async fn users_logout() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub(super) async fn ping() -> Response {
    StatusCode::OK.into_response()
}

pub(super) async fn device_description(State(state): State<AppState>) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><root xmlns=\"urn:schemas-upnp-org:device-1-0\"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType><friendlyName>crawler-media</friendlyName><manufacturer>crawler-media</manufacturer><modelName>crawler-media</modelName><UDN>uuid:{}</UDN><presentationURL>/</presentationURL></device></root>",
        state.provider.server_id()
    );
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

pub(super) async fn authenticate_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AuthBody>,
) -> Result<Json<Value>, StatusCode> {
    let res = match state
        .provider
        .authenticate_password(&body.username, &body.pw)
        .await
    {
        Ok(Some(res)) => res,
        Ok(None) => return Err(StatusCode::UNAUTHORIZED),
        Err(AuthenticateError::Busy) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        Err(AuthenticateError::ReadOnly) => return Err(StatusCode::CONFLICT),
        Err(AuthenticateError::Internal(message)) => {
            tracing::error!(%message, "Jellyfin 密码登录失败");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let client_id = crate::auth::extract_client_headers(&headers);
    if let Some(device_id) = client_id.device_id.as_deref() {
        state
            .provider
            .bind_playback_credential(res.1.id, device_id, &res.0, None)
            .await
            .map_err(|error| error.status_code())?;
    }

    let server_id = state.provider.server_id();
    let user = user_profile_json(&res.1, &server_id);
    let mut session_hash = std::collections::hash_map::DefaultHasher::new();
    (
        res.1.id,
        &client_id.device_id,
        &client_id.client,
        std::time::SystemTime::now(),
    )
        .hash(&mut session_hash);
    let session_id = format!("jf-{:016x}", session_hash.finish());
    let mut session_info = json!({
        "Id": session_id,
        "UserId": res.1.id.to_string(),
        "UserName": res.1.login,
        "ServerId": server_id,
        "IsActive": true,
        "SupportsMediaControl": true,
        "SupportsRemoteControl": false,
        "HasCustomDeviceName": client_id.device_name.is_some(),
        "PlayableMediaTypes": ["Video"],
        "SupportedCommands": [],
    });
    if let Some(device_id) = client_id.device_id {
        session_info["DeviceId"] = json!(device_id);
    }
    if let Some(client) = client_id.client {
        session_info["Client"] = json!(client);
    }
    if let Some(device_name) = client_id.device_name {
        session_info["DeviceName"] = json!(device_name);
    }
    if let Some(version) = client_id.version {
        session_info["ApplicationVersion"] = json!(version);
    }

    Ok(Json(json!({
        "AccessToken": res.0,
        "ServerId": server_id,
        "User": user,
        "SessionInfo": session_info,
    })))
}
pub(super) async fn quickconnect_enabled() -> Json<Value> {
    Json(json!(false))
}
