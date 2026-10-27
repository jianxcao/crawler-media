//! Resume, watched and favorite state endpoints.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::resolve_media_id;
use crate::http::store_wait::store_read;
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
    // Review L2: resume resolution reads the ledger and watch rows.
    match store_read(&state, move |store| -> Response {
        let (file, _media, resolved) =
            match super::selection::resolve_playback_for(store, &selection, user_id) {
                Ok(resolved) => resolved,
                // 无显式源时，没有可播文件的单元（含 TV whole 单元）仍返回已记录的进度
                Err(super::request::SelectionError::Unavailable) if selection.file_id.is_none() => {
                    let season = selection.season_number.map_or(UNIT_WHOLE, |v| v as i32);
                    let episode = selection.episode_number.map_or(UNIT_WHOLE, |v| v as i32);
                    let watch = match watch_json(
                        store,
                        user_id,
                        selection.media_item_id,
                        season,
                        episode,
                    ) {
                        Ok(watch) => watch,
                        Err(response) => return response,
                    };
                    return ok(watch).into_response();
                }
                Err(error) => return error.response(),
            };
        let unit = resolved.target.unit;
        let mut watch = match watch_json(
            store,
            user_id,
            selection.media_item_id,
            unit.season,
            unit.episode,
        ) {
            Ok(watch) => watch,
            Err(response) => return response,
        };
        watch["file_id"] = json!(file.file_id.to_string());
        watch["start_ms"] = json!(resolved.start_ms);
        watch["duration_ms"] = json!(resolved.duration_ms);
        watch["resume_status"] = json!(super::session::resume_status_name(resolved.status));
        ok(watch).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

/// GET /playback/marks — played/favorite + unplayed_count for a target.
pub(crate) async fn marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<MarkQuery>,
) -> Response {
    let Some(media_id) = query.media_item_id.as_deref().and_then(resolve_media_id) else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let (season, episode) = match mark_target(
        query.season_number.as_deref().map(query_coordinate),
        query.episode_number.as_deref().map(query_coordinate),
    ) {
        Ok(target) => target,
        Err(response) => return response,
    };
    match store_read(&state, move |store| -> Response {
        let Some((season, episode)) = super::selection::watch_unit(store, media_id, season, episode)
        else {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "电影只有整部单元，不接受季/集坐标",
            );
        };
        let rows = match store.unit_rows(user_id, media_id) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, %media_id, "读取播放记录失败");
                return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "读取播放状态失败");
            }
        };
        let watch = match watch_json(store, user_id, media_id, season, episode) {
            Ok(watch) => watch,
            Err(response) => return response,
        };
        ok(json!({
            "played": watch["played"].as_bool().unwrap_or(false),
            "is_favorite": favorite_for(store, user_id, media_id, season, episode, &rows),
            "unplayed_count": match unplayed_count(store, user_id, media_id, season, episode) {
                Ok(count) => count,
                Err(error) => {
                    tracing::error!(%error, %media_id, "读取未看分集失败");
                    return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "读取播放状态失败");
                }
            },
        }))
        .into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}

/// The effective watch state of one target, or the failure response to send back.
///
/// A store read that failed must not be flattened into `played: false`: the client
/// would then show the opposite of what the user marked.
fn watch_json(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Value, Response> {
    super::effective_watch_json(store, user_id, media_id, season, episode).map_err(|error| {
        tracing::error!(%error, %media_id, season, episode, "读取播放进度失败");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            "读取播放状态失败",
        )
    })
}

/// A mark names one of three targets: the whole item, one season, or one episode.
/// A missing coordinate is whole (`-1`); a present one must fit a nonnegative i32,
/// including the legal TV special `0`. Strings, negatives and overflow are refused.
fn mark_target(
    season: Option<Result<u64, ()>>,
    episode: Option<Result<u64, ()>>,
) -> Result<(i32, i32), Response> {
    let season = coordinate("season_number", season)?;
    let episode = coordinate("episode_number", episode)?;
    match (season, episode) {
        (None, Some(_)) => Err(err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "episode_number 不能单独出现",
        )),
        (season, episode) => Ok((season.unwrap_or(UNIT_WHOLE), episode.unwrap_or(UNIT_WHOLE))),
    }
}

fn query_coordinate(raw: &str) -> Result<u64, ()> {
    raw.parse::<u64>().map_err(|_| ())
}

fn coordinate(name: &str, raw: Option<Result<u64, ()>>) -> Result<Option<i32>, Response> {
    let Some(raw) = raw else { return Ok(None) };
    let Ok(value) = raw.and_then(|value| i32::try_from(value).map_err(|_| ())) else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            &format!("{name} 必须是 0 到 2147483647 的整数"),
        ));
    };
    Ok(Some(value))
}

fn json_coordinate(value: Option<&Value>) -> Option<Result<u64, ()>> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(number.as_u64().ok_or(())),
        Some(_) => Some(Err(())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarkQuery {
    media_item_id: Option<String>,
    season_number: Option<String>,
    episode_number: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkInput {
    media_item_id: Option<String>,
    #[serde(default)]
    season_number: Option<Value>,
    #[serde(default)]
    episode_number: Option<Value>,
    #[serde(default)]
    played: Option<bool>,
    #[serde(default)]
    favorite: Option<bool>,
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

fn unplayed_count(
    store: &crate::Store,
    user: domain::UserId,
    media: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Option<i64>, crate::store::StoreError> {
    if episode != UNIT_WHOLE {
        return Ok(None);
    }
    let visible = crate::visible_scope::VisibleLibraries::load(store, Some(user))?;
    let coverage = visible.media_coverage(store, media, |_| true)?;
    let units: std::collections::HashSet<_> = coverage
        .iter()
        .flat_map(|row| &row.units)
        .filter(|unit| {
            unit.season >= 0 && unit.episode >= 0 && (season == UNIT_WHOLE || unit.season == season)
        })
        .copied()
        .collect();
    // The count is over owned episodes, not over the user's sparse rows: a season
    // mark writes one assertion and leaves every episode row absent. Both reads are
    // batched per Media, so a long season costs the same as a short one.
    let units: Vec<domain::UnitKey> = units.into_iter().collect();
    let states = store.effective_unit_states(user, media, &units)?;
    let count = states
        .iter()
        .filter(|state| !state.as_ref().is_some_and(|state| state.played))
        .count();
    Ok((count > 0).then_some(count as i64))
}

/// POST /playback/marks — set played (cascade) / favorite (target level).
pub(crate) async fn set_marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    if body.as_object().is_some_and(|object| {
        object
            .keys()
            .any(|key| matches!(key.as_str(), "season" | "episode"))
    }) {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "marks 不接受旧坐标字段",
        );
    }
    let body: MarkInput = match serde_json::from_value(body) {
        Ok(body) => body,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                &error.to_string(),
            );
        }
    };
    let Some(media_id) = body.media_item_id.as_deref().and_then(resolve_media_id) else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let (season, episode) = match mark_target(
        json_coordinate(body.season_number.as_ref()),
        json_coordinate(body.episode_number.as_ref()),
    ) {
        Ok(target) => target,
        Err(response) => return response,
    };
    // Review L2: a mark is a Store mutation; the executor must not wait on the
    // global lock for it.
    let now_ms = state.playback_clock.now_ms();
    match store_read(&state, move |store| -> Response {
        let Some((season, episode)) =
            super::selection::watch_unit(store, media_id, season, episode)
        else {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "电影只有整部单元，不接受季/集坐标",
            );
        };
        let played = body.played;
        let favorite = body.favorite;
        let change = crate::store::playback::PlaybackMutation::marks(
            user_id, season, episode, played, favorite,
        );
        if let Err(error) = store.mutate_playback(media_id, &change, now_ms) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        let unit = match watch_json(store, user_id, media_id, season, episode) {
            Ok(unit) => unit,
            Err(response) => return response,
        };
        ok(json!({
            "played": unit["played"].as_bool().unwrap_or(false),
            "is_favorite": unit["is_favorite"].as_bool().unwrap_or(false),
            "unplayed_count": null,
        }))
        .into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.response(),
    }
}
