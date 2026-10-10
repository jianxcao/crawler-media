use super::AppState;
use crate::auth::AuthUser;
use crate::provider::PlaybackEvent;
use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct ProgressBody {
    #[serde(default, rename = "ItemId")]
    item_id: Option<String>,
    #[serde(default, rename = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(default, rename = "IsPaused")]
    is_paused: Option<bool>,
}

async fn report_session_event(
    state: AppState,
    user: AuthUser,
    body: ProgressBody,
    headers: HeaderMap,
    event: PlaybackEvent,
    paused: bool,
) -> StatusCode {
    let (Some(item_id), Some(position_ticks)) = (body.item_id.as_deref(), body.position_ticks)
    else {
        return StatusCode::NO_CONTENT;
    };
    let position_ms = (position_ticks / 10_000).max(0);
    let client = user.playback_client(&headers);
    match state
        .provider
        .is_device_revoked(user.id, &client.device_id)
        .await
    {
        Ok(true) => return StatusCode::FORBIDDEN,
        Ok(false) => {}
        Err(error) => {
            tracing::error!(%error, user_id = %user.id, device_id = %client.device_id, "Jellyfin device revocation lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }
    match state
        .provider
        .report_playback_event(user.id, item_id, position_ms, paused, client, event)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::error!(
                %error,
                user_id = %user.id,
                item_id,
                ?event,
                "failed to persist Jellyfin playback event"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub(super) async fn session_playing(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    report_session_event(state, user, body, headers, PlaybackEvent::Playing, false).await
}

pub(super) async fn session_progress(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    let paused = body.is_paused.unwrap_or(false);
    report_session_event(state, user, body, headers, PlaybackEvent::Progress, paused).await
}

pub(super) async fn session_stopped(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    report_session_event(state, user, body, headers, PlaybackEvent::Stopped, true).await
}
