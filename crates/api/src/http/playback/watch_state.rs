//! Resume, watched and favorite state endpoints.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::resolve_media_id;
use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::{UNIT_WHOLE, UnitRow};

/// GET /playback/resume — source-aware resume preview. Interprets the same optional
/// source preference as `POST /playback/sessions` through `Store::resolve_playback`,
/// without opening a session.
pub(crate) async fn resume(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut body = serde_json::Map::new();
    for key in ["media_item_id", "file_id", "season", "episode"] {
        if let Some(value) = query.get(key) {
            body.insert(key.into(), Value::String(value.clone()));
        }
    }
    for key in ["season_number", "episode_number"] {
        if let Some(value) = query.get(key) {
            let Ok(number) = value.parse::<u64>() else {
                return super::request::SelectionError::Invalid.response();
            };
            body.insert(key.into(), json!(number));
        }
    }
    let selection = match super::request::parse_selection(&Value::Object(body)) {
        Ok(selection) => selection,
        Err(error) => return error.response(),
    };
    let store = state.store.lock();
    let (file, _media, resolved) =
        match super::selection::resolve_playback_for(&store, &selection, user_id) {
            Ok(resolved) => resolved,
            // 无显式源时，没有可播文件的单元（含 TV whole 单元）仍返回已记录的进度
            Err(super::request::SelectionError::Unavailable) if selection.file_id.is_none() => {
                let season = selection.season_number.map_or(UNIT_WHOLE, |v| v as i32);
                let episode = selection.episode_number.map_or(UNIT_WHOLE, |v| v as i32);
                return ok(super::effective_watch_json(
                    &store,
                    user_id,
                    selection.media_item_id,
                    season,
                    episode,
                ))
                .into_response();
            }
            Err(error) => return error.response(),
        };
    let unit = resolved.target.unit;
    let mut watch = super::effective_watch_json(
        &store,
        user_id,
        selection.media_item_id,
        unit.season,
        unit.episode,
    );
    watch["file_id"] = json!(file.file_id.to_string());
    watch["start_ms"] = json!(resolved.start_ms);
    watch["duration_ms"] = json!(resolved.duration_ms);
    watch["resume_status"] = json!(super::session::resume_status_name(resolved.status));
    ok(watch).into_response()
}

/// GET /playback/marks — played/favorite + unplayed_count for a target.
pub(crate) async fn marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(media_id) = query.get("media_item_id").and_then(|v| resolve_media_id(v)) else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let season = query
        .get("season_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let episode = query
        .get("episode_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let store = state.store.lock();
    let Some((season, episode)) = super::selection::watch_unit(&store, media_id, season, episode)
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "电影只有整部单元，不接受季/集坐标",
        );
    };
    let rows = store.unit_rows(user_id, media_id).unwrap_or_default();
    let watch = super::effective_watch_json(&store, user_id, media_id, season, episode);
    ok(json!({
        "played": watch["played"].as_bool().unwrap_or(false),
        "is_favorite": favorite_for(&store, user_id, media_id, season, episode, &rows),
        "unplayed_count": unplayed_count(&rows, season, episode),
    }))
    .into_response()
}

fn favorite_for(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    rows: &[UnitRow],
) -> bool {
    if season == UNIT_WHOLE && episode == UNIT_WHOLE {
        if let Some(true) = explicit_favorite(store, user_id, media_id, season, episode) {
            return true;
        }
        return rows.iter().any(|row| row.favorite);
    }
    if let Some(favorite) = explicit_favorite(store, user_id, media_id, season, episode) {
        return favorite;
    }
    inherited_favorite(store, user_id, media_id, season, episode)
}

fn explicit_favorite(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Option<bool> {
    store
        .unit_state(user_id, media_id, season, episode)
        .ok()
        .flatten()
        .map(|unit| unit.favorite)
}

fn inherited_favorite(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    if explicit_favorite(store, user_id, media_id, UNIT_WHOLE, UNIT_WHOLE) == Some(true) {
        return true;
    }
    season >= 0
        && episode >= 0
        && explicit_favorite(store, user_id, media_id, season, UNIT_WHOLE) == Some(true)
}

fn unplayed_count(rows: &[UnitRow], season: i32, episode: i32) -> Option<i64> {
    if episode != UNIT_WHOLE {
        return None;
    }
    let count = rows
        .iter()
        .filter(|row| {
            row.episode >= 0 && (season == UNIT_WHOLE || row.season == season) && !row.played
        })
        .count();
    (count > 0).then_some(count as i64)
}

/// POST /playback/marks — set played (cascade) / favorite (target level).
pub(crate) async fn set_marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let Some(media_id) = body["media_item_id"]
        .as_str()
        .and_then(|v| resolve_media_id(v))
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let season = body["season_number"].as_i64().unwrap_or(-1) as i32;
    let episode = body["episode_number"].as_i64().unwrap_or(-1) as i32;
    let store = state.store.lock();
    let Some((season, episode)) = super::selection::watch_unit(&store, media_id, season, episode)
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "电影只有整部单元，不接受季/集坐标",
        );
    };
    let played = body["played"].as_bool();
    let favorite = body["favorite"].as_bool();
    // `playback_clock` 是毫秒时钟；`playback_units.updated_at` 存 unix 秒。
    let now = state.playback_clock.now_ms() / 1000;
    if let Err(error) =
        store.set_unit_marks(user_id, media_id, season, episode, played, favorite, now)
    {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let unit = super::effective_watch_json(&store, user_id, media_id, season, episode);
    ok(json!({
        "played": unit["played"].as_bool().unwrap_or(false),
        "is_favorite": unit["is_favorite"].as_bool().unwrap_or(false),
        "unplayed_count": null,
    }))
    .into_response()
}
