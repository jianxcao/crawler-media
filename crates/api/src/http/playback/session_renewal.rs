use axum::{http::StatusCode, response::Response};
use serde_json::Value;
use store::playback::session_ids::PlaybackRenewalError;

use super::request::SelectionError;
use crate::http::err;

pub(super) fn previous_session(body: &Value) -> Result<Option<String>, SelectionError> {
    match body.get("resume_from_play_session_id") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
            .map(|id| Some(id.to_string()))
            .ok_or(SelectionError::Invalid),
    }
}

pub(super) fn renewal_error(error: PlaybackRenewalError) -> Response {
    match error {
        PlaybackRenewalError::PreviousUnavailable => err(
            StatusCode::NOT_FOUND,
            "playback.session_missing",
            "原播放会话已不可用，请重新开始播放",
        ),
        PlaybackRenewalError::TargetChanged => err(
            StatusCode::CONFLICT,
            "playback.resume_target_changed",
            "播放目标已改变，请重新开始播放",
        ),
        PlaybackRenewalError::Superseded => err(
            StatusCode::CONFLICT,
            "playback.resume_superseded",
            "此设备已开始其他播放，请重新开始播放",
        ),
        PlaybackRenewalError::EndedByAdmin => err(
            StatusCode::CONFLICT,
            "playback.ended_by_admin",
            "管理员已结束本次播放",
        ),
        PlaybackRenewalError::Store(
            store::StoreError::WriteBusy
            | store::StoreError::ReadBusy
            | store::StoreError::ReadClosed
            | store::StoreError::ReadDeadline
            | store::StoreError::ReadCancelled,
        ) => crate::http::store_wait::busy_response(),
        PlaybackRenewalError::Store(store::StoreError::Protected(ref message))
            if message.contains("revoked") =>
        {
            err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销")
        }
        PlaybackRenewalError::Store(
            store::StoreError::Missing(_) | store::StoreError::Protected(_),
        ) => SelectionError::Unavailable.response(),
        PlaybackRenewalError::Store(error) => {
            tracing::error!(%error, "Playback page session renewal failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "无法恢复播放会话",
            )
        }
    }
}
