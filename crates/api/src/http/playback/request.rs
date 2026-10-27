//! The one shared playback-selection contract for self-owned playback entries.
//!
//! Only the selection fields are parsed here; device/codec/session parameters stay with
//! their handlers. Omitted or `null` fields mean "choose automatically"; any present but
//! malformed field is `Invalid`, never silently downgraded to automatic selection.

use axum::http::StatusCode;
use axum::response::Response;
use domain::{LedgerId, MediaId};
use serde_json::Value;

use crate::http::err;

/// Coordinates and optional explicit source named by a playback request.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlaybackSelection {
    pub media_item_id: MediaId,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub file_id: Option<LedgerId>,
    pub start_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionError {
    /// Malformed request (400).
    Invalid,
    /// Not found, not visible, not present, or not covering the target (404, no leak).
    Unavailable,
}

impl SelectionError {
    pub(super) fn response(self) -> Response {
        match self {
            Self::Invalid => err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "播放选择参数无效",
            ),
            Self::Unavailable => err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在"),
        }
    }
}

/// Fields removed from the contract; naming them is a client bug, not an alias.
const OBSOLETE_KEYS: [&str; 2] = ["season", "episode"];

pub(super) fn parse_selection(body: &Value) -> Result<PlaybackSelection, SelectionError> {
    if OBSOLETE_KEYS.iter().any(|key| body.get(key).is_some()) {
        return Err(SelectionError::Invalid);
    }
    let media_item_id = body
        .get("media_item_id")
        .and_then(Value::as_str)
        .and_then(super::resolve_media_id)
        .ok_or(SelectionError::Invalid)?;
    let file_id = match body.get("file_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(|raw| raw.replace('-', "").parse::<LedgerId>().ok())
                .ok_or(SelectionError::Invalid)?,
        ),
    };
    // `start_ms: null` is not "omitted": a client that sends a key means a value.
    let start_ms = match body.get("start_ms") {
        None => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|start| *start >= 0)
                .ok_or(SelectionError::Invalid)?,
        ),
    };
    Ok(PlaybackSelection {
        media_item_id,
        season_number: coordinate(body, "season_number")?,
        episode_number: coordinate(body, "episode_number")?,
        file_id,
        start_ms,
    })
}

fn coordinate(body: &Value, key: &str) -> Result<Option<u32>, SelectionError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| i32::try_from(*n).is_ok())
            .map(Some)
            .ok_or(SelectionError::Invalid),
    }
}
