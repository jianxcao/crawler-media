//! Resume, watched and favorite state endpoints.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::resolve_media_id;
use crate::http::store_wait::{store_read, store_write};
use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::{UNIT_WHOLE, UnitRow};

fn item_missing() -> Response {
    err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在")
}

fn read_store_error(error: crate::store::StoreError) -> Response {
    if matches!(
        error,
        crate::store::StoreError::WriteBusy
            | crate::store::StoreError::ReadBusy
            | crate::store::StoreError::ReadClosed
            | crate::store::StoreError::ReadDeadline
            | crate::store::StoreError::ReadCancelled
    ) {
        return crate::http::store_wait::busy_response();
    }
    tracing::error!(%error, "读取播放状态失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "读取播放状态失败",
    )
}

fn visible_published_media(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
) -> Result<Option<(domain::Media, Vec<domain::FileCoverage>)>, Response> {
    let Some(media) = store.get_media(media_id).map_err(read_store_error)? else {
        return Ok(None);
    };
    let visible = crate::visible_scope::VisibleLibraries::load(store, Some(user_id))
        .map_err(read_store_error)?;
    let coverages = visible
        .media_coverage(store, media_id, |_| true)
        .map_err(read_store_error)?;
    if coverages.is_empty() {
        return Ok(None);
    }
    Ok(Some((media, coverages)))
}

fn resume_read(
    store: &crate::Store,
    user_id: domain::UserId,
    selection: &super::request::PlaybackSelection,
) -> Response {
    let Some((media, _)) = (match visible_published_media(store, user_id, selection.media_item_id) {
        Ok(value) => value,
        Err(response) => return response,
    }) else {
        return item_missing();
    };
    if let Some(file_id) = selection.file_id {
        match store.visible_file(user_id, file_id) {
            Ok(Some(file)) if file.media_id == media.id => {}
            Ok(_) => return item_missing(),
            Err(error) => return read_store_error(error),
        }
    }
    if media.kind != domain::MediaKind::Tv
        && (selection.season_number.is_some() || selection.episode_number.is_some())
    {
        return super::request::SelectionError::Invalid.response();
    }
    if media.kind == domain::MediaKind::Tv
        && (selection.season_number.is_some() ^ selection.episode_number.is_some())
    {
        return super::request::SelectionError::Invalid.response();
    }
    let tv_whole = selection.file_id.is_none()
        && selection.season_number.is_none()
        && selection.episode_number.is_none()
        && media.kind == domain::MediaKind::Tv;
    if tv_whole {
        return match watch_json(
            store,
            user_id,
            selection.media_item_id,
            UNIT_WHOLE,
            UNIT_WHOLE,
        ) {
            Ok(watch) => ok(watch).into_response(),
            Err(response) => response,
        };
    }
    let (file, _media, resolved) =
        match super::selection::resolve_playback_for(store, selection, user_id) {
            Ok(resolved) => resolved,
            // 无显式源时，没有可播文件的单元（含 TV whole 单元）仍返回已记录的进度
            Err(super::request::SelectionError::Unavailable) if selection.file_id.is_none() => {
                let season = selection.season_number.map_or(UNIT_WHOLE, |v| v as i32);
                let episode = selection.episode_number.map_or(UNIT_WHOLE, |v| v as i32);
                return match watch_json(store, user_id, selection.media_item_id, season, episode) {
                    Ok(watch) => ok(watch).into_response(),
                    Err(response) => response,
                };
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
}

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
    match store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| -> Response { resume_read(store, user_id, &selection) },
    )
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
    match store_read(&state, &[store::Db::Library, store::Db::App], move |store| -> Response {
        let Some((media, coverages)) = (match visible_published_media(store, user_id, media_id) {
            Ok(value) => value,
            Err(response) => return response,
        }) else {
            return item_missing();
        };
        let Some((season, episode)) =
            super::selection::watch_unit_for_kind(media.kind, season, episode)
        else {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "电影只有整部单元，不接受季/集坐标",
            );
        };
        let rows = match store.unit_rows(user_id, media_id) {
            Ok(rows) => rows,
            Err(error) => return read_store_error(error),
        };
        let watch = match watch_json(store, user_id, media_id, season, episode) {
            Ok(watch) => watch,
            Err(response) => return response,
        };
        ok(json!({
            "played": watch["played"].as_bool().unwrap_or(false),
            "is_favorite": favorite_for(store, user_id, media_id, season, episode, &rows),
            "unplayed_count": match unplayed_count_for_coverages(store, user_id, media_id, season, episode, &coverages) {
                Ok(count) => count,
                Err(error) => return read_store_error(error),
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

fn unplayed_count_for_coverages(
    store: &crate::Store,
    user: domain::UserId,
    media: domain::MediaId,
    season: i32,
    episode: i32,
    coverage: &[domain::FileCoverage],
) -> Result<Option<i64>, crate::store::StoreError> {
    if episode != UNIT_WHOLE {
        return Ok(None);
    }
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

fn apply_set_marks(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    played: Option<bool>,
    favorite: Option<bool>,
    now_ms: i64,
) -> Response {
    let media = match visible_published_media(store, user_id, media_id) {
        Ok(Some((media, _))) => Some(media),
        Ok(None) => None,
        Err(response) => return response,
    };
    let (season, episode) = if let Some(media) = media {
        let Some(unit) = super::selection::watch_unit_for_kind(media.kind, season, episode) else {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "电影只有整部单元，不接受季/集坐标",
            );
        };
        unit
    } else {
        match store.get_media(media_id) {
            Ok(None) => return item_missing(),
            Ok(Some(_)) => {}
            Err(error) => return mark_error(error),
        }
        // Zero-write barrier check: retired/merging media returns 409 playback.protected
        // without leaking MediaKind, while hidden/unpublished returns 404 playback.item_missing.
        let barrier = crate::store::playback::PlaybackMutation::marks(
            user_id, UNIT_WHOLE, UNIT_WHOLE, None, None,
        );
        if let Err(error) = store.mutate_playback(media_id, &barrier, now_ms) {
            return mark_error(error);
        }
        let Some(unit) = super::selection::watch_unit(store, media_id, season, episode) else {
            return err(
                StatusCode::BAD_REQUEST,
                "playback.invalid",
                "电影只有整部单元，不接受季/集坐标",
            );
        };
        unit
    };
    let change =
        crate::store::playback::PlaybackMutation::marks(user_id, season, episode, played, favorite);
    if let Err(error) = store.mutate_playback(media_id, &change, now_ms) {
        return mark_error(error);
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
    let played = body.played;
    let favorite = body.favorite;
    match store_write(&state, move |store| -> Response {
        apply_set_marks(
            store, user_id, media_id, season, episode, played, favorite, now_ms,
        )
    })
    .await
    {
        Ok(response) => response,
        Err(response) => response,
    }
}

fn mark_error(error: crate::store::StoreError) -> Response {
    match error {
        crate::store::StoreError::Protected(message) => {
            tracing::warn!(%message, "Playback mark rejected by write barrier");
            err(StatusCode::CONFLICT, "playback.protected", &message)
        }
        crate::store::StoreError::Missing(_) => item_missing(),
        crate::store::StoreError::WriteBusy
        | crate::store::StoreError::ReadBusy
        | crate::store::StoreError::ReadClosed
        | crate::store::StoreError::ReadDeadline
        | crate::store::StoreError::ReadCancelled => crate::http::store_wait::busy_response(),
        other => {
            tracing::error!(error = %other, "Playback mark persistence failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &other.to_string(),
            )
        }
    }
}
