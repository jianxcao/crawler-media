use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::auth::{AuthUser, authenticate};
use crate::dto::{item_dto_json, library_view_json, user_profile_json};
use crate::provider::{MediaServerProvider, PlaybackClientInfo, PlaybackEvent};
mod item_filters;
mod media;
mod people;
mod websocket;
use item_filters::filter_items;
use media::playback_info;

#[derive(Clone)]
pub struct AppState {
    pub provider: Arc<dyn MediaServerProvider>,
    data_changes: broadcast::Sender<(domain::UserId, Value)>,
}

impl AppState {
    pub(super) fn new(provider: Arc<dyn MediaServerProvider>) -> Self {
        let (data_changes, _) = broadcast::channel(128);
        Self {
            provider,
            data_changes,
        }
    }
}

#[derive(Default, Deserialize)]
pub struct ItemsQuery {
    #[serde(default, rename = "ParentId", alias = "parentId")]
    pub parent_id: Option<String>,
    #[serde(default, rename = "StartIndex", alias = "startIndex")]
    pub start_index: Option<usize>,
    #[serde(default, rename = "Limit", alias = "limit")]
    pub limit: Option<usize>,
    #[serde(default, rename = "SeriesId", alias = "seriesId")]
    pub series_id: Option<String>,
    #[serde(default, rename = "SeasonId", alias = "seasonId")]
    pub season_id: Option<String>,
    #[serde(
        default,
        rename = "PersonIds",
        alias = "personIds",
        alias = "PersonId",
        alias = "personId"
    )]
    pub person_ids: Option<String>,
    #[serde(default, rename = "Filters", alias = "filters")]
    pub filters: Option<String>,
    #[serde(default, rename = "IsFavorite", alias = "isFavorite")]
    pub is_favorite: Option<bool>,
    #[serde(default, rename = "IsPlayed", alias = "isPlayed")]
    pub is_played: Option<bool>,
    #[serde(default, rename = "SortBy", alias = "sortBy")]
    pub sort_by: Option<String>,
    #[serde(default, rename = "SortOrder", alias = "sortOrder")]
    pub sort_order: Option<String>,
    #[serde(default, rename = "IncludeItemTypes", alias = "includeItemTypes")]
    pub include_item_types: Option<String>,
}

#[derive(Default, Deserialize)]
struct ViewsQuery {
    #[serde(default, rename = "IncludeHidden", alias = "includeHidden")]
    include_hidden: Option<bool>,
}

#[derive(Deserialize)]
struct UserItemPath {
    user_id: Option<String>,
    id: String,
}

async fn item_id_for_user(
    provider: &dyn MediaServerProvider,
    path: UserItemPath,
    user_id: domain::UserId,
) -> Result<String, StatusCode> {
    if let Some(path_user) = path.user_id {
        let matches_id = path_user
            .replace('-', "")
            .eq_ignore_ascii_case(&user_id.to_string().replace('-', ""));
        if !matches_id && !path_user.eq_ignore_ascii_case("me") {
            let user = provider
                .get_user(user_id)
                .await
                .map_err(|error| {
                    tracing::error!(%error, %user_id, "failed to resolve Jellyfin user path");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
                .ok_or(StatusCode::NOT_FOUND)?;
            if !user.login.eq_ignore_ascii_case(&path_user) {
                return Err(StatusCode::NOT_FOUND);
            }
        }
    }
    Ok(path.id)
}

#[derive(Deserialize)]
pub struct AuthBody {
    #[serde(alias = "Username")]
    pub username: String,
    #[serde(alias = "Pw")]
    pub pw: String,
}

pub fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let state = AppState::new(provider.clone());

    let protected = Router::new()
        .route("/UserViews", get(user_views))
        .route("/Users/{user_id}/Views", get(user_views))
        .route("/Users/{user_id}", get(user_profile))
        .route("/Users/Me", get(user_profile))
        .route("/Items/Counts", get(item_counts))
        .route("/Users/{user_id}/Items/Counts", get(item_counts))
        .route("/Items", get(items))
        .route("/socket", get(websocket::connect))
        .route("/Persons/{name}", get(people::person_by_name))
        .route("/Users/{user_id}/Items", get(items))
        .route("/Users/{user_id}/Items/Resume", get(resume_items))
        .route("/Users/{user_id}/Items/Latest", get(latest_items))
        .route("/Shows/NextUp", get(shows_next_up))
        .route("/Shows/{id}/Seasons", get(show_seasons))
        .route("/Shows/{id}/Episodes", get(show_episodes))
        .route("/Items/{id}", get(item_by_id))
        .route("/Users/{user_id}/Items/{id}", get(item_by_id))
        .route(
            "/Users/{user_id}/FavoriteItems/{id}",
            post(mark_favorite).delete(unmark_favorite),
        )
        .route(
            "/UserFavoriteItems/{id}",
            post(mark_favorite).delete(unmark_favorite),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{id}",
            post(mark_played).delete(unmark_played),
        )
        .route(
            "/UserPlayedItems/{id}",
            post(mark_played).delete(unmark_played),
        )
        .route("/Items/{id}/PlaybackInfo", post(playback_info))
        .route("/QuickConnect/Enabled", get(quickconnect_enabled))
        .route(
            "/DisplayPreferences/{id}",
            get(display_preferences).post(update_display_preferences),
        )
        .route("/Sessions/Playing", post(session_playing))
        .route("/Sessions/Playing/Progress", post(session_progress))
        .route("/Sessions/Playing/Stopped", post(session_stopped))
        .route_layer(axum::middleware::from_fn(move |req, next| {
            let p = provider.clone();
            authenticate(p, req, next)
        }))
        .with_state(state.clone());

    let public = Router::new()
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
        .route("/System/Info/Public", get(public_system_info))
        .route("/System/Ping", get(ping).post(ping))
        .route("/jellyfin", get(device_description))
        .with_state(state);

    let root = Router::new().merge(public).merge(protected);
    Router::new().merge(root.clone()).nest("/emby", root)
}

pub fn media_routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    media::routes(provider)
}

// Handlers

async fn public_system_info(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "Id": state.provider.server_id(),
        "ServerName": "crawler-media",
        "ProductName": "crawler-media",
        "Version": "0.1.0",
        "StartupWizardCompleted": true,
    }))
}

async fn ping() -> Response {
    StatusCode::OK.into_response()
}

async fn device_description(State(state): State<AppState>) -> Response {
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

async fn authenticate_by_name(
    State(state): State<AppState>,
    Json(body): Json<AuthBody>,
) -> Result<Json<Value>, StatusCode> {
    let res = state
        .provider
        .authenticate_password(&body.username, &body.pw)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(Json(json!({
        "AccessToken": res.0,
        "User": { "Id": res.1.id.to_string(), "Name": res.1.login },
    })))
}

async fn user_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let user_row = state
        .provider
        .get_user(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(user_profile_json(
        &user_row,
        &state.provider.server_id(),
    )))
}

async fn user_views(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ViewsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let server_id = state.provider.server_id();
    let include_hidden = query.include_hidden.unwrap_or(false);
    let views: Vec<Value> = libraries
        .iter()
        .filter(|lib| include_hidden || !lib.exclude_from_home)
        .map(|lib| library_view_json(lib, &server_id))
        .collect();
    let total = views.len();
    Ok(Json(json!({
        "Items": views,
        "TotalRecordCount": total,
    })))
}

async fn item_counts(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let items = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut movie_count = 0;
    let mut series_count = 0;
    for item in &items {
        if item.media.kind == domain::MediaKind::Movie {
            movie_count += 1;
        } else if item.is_series {
            series_count += 1;
        }
    }
    let mut episode_count = 0;
    for series in items.iter().filter(|item| item.is_series) {
        let series_id = series.media.id.to_string().replace('-', "");
        episode_count += state
            .provider
            .list_visible_items(user.id, Some(&series_id))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .len();
    }
    Ok(Json(json!({
        "MovieCount": movie_count,
        "SeriesCount": series_count,
        "EpisodeCount": episode_count,
        "ItemCount": items.len(),
    })))
}

async fn items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let parent_id = query.parent_id.as_deref().or(query.series_id.as_deref());
    let mut rows = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if query.person_ids.is_some() {
        let mut enriched = Vec::with_capacity(rows.len());
        for row in rows {
            enriched.push(state.provider.enrich_visible_item(row).await);
        }
        rows = enriched;
    }
    let rows = filter_items(rows, &query);
    let episodes_only = !rows.is_empty()
        && rows
            .iter()
            .all(|item| item.media.kind == domain::MediaKind::Tv && !item.is_series);
    let (default_sort, default_order) = if episodes_only {
        ("ParentIndexNumber,IndexNumber", "Ascending")
    } else {
        ("DateCreated,SortName", "Descending")
    };
    let rows = item_filters::sort_items(
        rows,
        query.sort_by.as_deref().unwrap_or(default_sort),
        query.sort_order.as_deref().unwrap_or(default_order),
    );
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn resume_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let root_rows = state
        .provider
        .list_visible_items(user.id, query.parent_id.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut rows = Vec::new();
    for item in root_rows {
        if item.media.kind == domain::MediaKind::Tv && item.is_series {
            let series_id = item.media.id.to_string().replace('-', "");
            let mut episodes = state
                .provider
                .list_visible_items(user.id, Some(&series_id))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            episodes.retain(|episode| episode.ticks > 0 && !episode.played);
            episodes.sort_by_key(|episode| {
                (
                    episode.metadata.resume_updated_at,
                    episode.row.season.unwrap_or_default(),
                    episode.row.episode.unwrap_or_default(),
                )
            });
            if let Some(episode) = episodes.pop() {
                rows.push(episode);
            }
        } else if item.ticks > 0 && !item.played {
            rows.push(item);
        }
    }
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = rows
        .drain(..)
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn latest_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let rows = state
        .provider
        .list_visible_items(user.id, query.parent_id.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 「最近入库」的播放状态口径（见 library::latest）：
    // 显式给了 IsPlayed 就按它严格筛（协议语义原样保留）；没给时**只排不筛**——
    // 没看完的在前、已看完的沉底，段内仍是入库时间倒序。
    // Jellyfin 在缺省位置会因为 HidePlayedInLatest 直接把已看完的藏掉，全看过的库
    // 更是连入口都没有；我们改成排到最后，客户端那一行于是永远有内容。
    let rows = item_filters::filter_items_except_played(rows, &query);
    let rows = item_filters::sort_items(rows, "DateCreated,SortName", "Descending");
    let rows = match query.is_played {
        Some(played) => rows.into_iter().filter(|item| item.played == played).collect(),
        None => library::played_last(rows, |item| item.played),
    };
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!(listed)))
}

async fn shows_next_up(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let series = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let mut next_up = Vec::new();
    for show in series
        .iter()
        .filter(|item| item.media.kind == domain::MediaKind::Tv && item.is_series)
    {
        let series_id = show.media.id.to_string().replace('-', "");
        let mut episodes = state
            .provider
            .list_visible_items(user.id, Some(&series_id))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        episodes.sort_by_key(|item| {
            (
                item.row.season.unwrap_or_default(),
                item.row.episode.unwrap_or_default(),
            )
        });
        if let Some(episode) = episodes.into_iter().find(|item| !item.played) {
            next_up.push(item_dto_json(episode));
        }
    }
    let total = next_up.len();
    let start_index = query.start_index.unwrap_or(0);
    let listed: Vec<Value> = next_up.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn show_seasons(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let item = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let compact_id = item.media.id.to_string().replace('-', "");
    let episodes = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut season_numbers: std::collections::BTreeSet<u32> = episodes
        .into_iter()
        .map(|episode| episode.row.season.unwrap_or(1))
        .collect();
    if season_numbers.is_empty() {
        season_numbers.insert(item.row.season.unwrap_or(1));
    }
    let server_id = state.provider.server_id();
    let seasons_json: Vec<Value> = season_numbers
        .into_iter()
        .map(|season_num| {
            json!({
                    "Id": format!("{compact_id}{season_num:02}"),
            "Name": format!("第 {} 季", season_num),
            "SeriesId": compact_id,
            "SeriesName": item.media.title,
            "IndexNumber": season_num,
            "Type": "Season",
            "IsFolder": true,
                    "ServerId": server_id,
                })
        })
        .collect();
    Ok(Json(
        json!({ "Items": seasons_json, "TotalRecordCount": seasons_json.len() }),
    ))
}

async fn show_episodes(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let compact_id = id.replace('-', "");
    let rows = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let season_number = query
        .season_id
        .as_deref()
        .and_then(|season_id| parse_season_id(&compact_id, season_id));
    let limit = query.limit.unwrap_or(200);
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|episode| match season_number {
            Some(season) => episode.row.season == Some(season),
            None => query.season_id.is_none(),
        })
        .collect();
    rows.sort_by_key(|episode| {
        (
            episode.row.season.unwrap_or_default(),
            episode.row.episode.unwrap_or_default(),
        )
    });
    let total = rows.len();
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(query.start_index.unwrap_or(0))
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

fn parse_season_id(series_id: &str, season_id: &str) -> Option<u32> {
    let series_id = series_id.replace('-', "");
    let season_id = season_id.replace('-', "");
    let suffix = season_id.get(series_id.len()..)?;
    if !season_id[..series_id.len()].eq_ignore_ascii_case(&series_id) {
        return None;
    }
    suffix.parse().ok()
}

async fn item_by_id(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let snapshot = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(item_dto_json(snapshot)))
}

async fn user_data_response(
    state: &AppState,
    user_id: domain::UserId,
    item_id: &str,
) -> Result<Json<Value>, StatusCode> {
    let snapshot = state
        .provider
        .resolve_user_item_data(user_id, item_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user_id, %item_id, "failed to resolve Jellyfin user data response");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(snapshot.json_value()))
}

async fn mark_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, None, Some(true))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, favorite = true, "updated Jellyfin favorite mark");
    websocket::publish_user_data_changed(&state, user.id, &id).await;
    user_data_response(&state, user.id, &id).await
}

async fn unmark_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, None, Some(false))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, favorite = false, "updated Jellyfin favorite mark");
    websocket::publish_user_data_changed(&state, user.id, &id).await;
    user_data_response(&state, user.id, &id).await
}

async fn mark_played(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, Some(true), None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, played = true, "updated Jellyfin played mark");
    websocket::publish_user_data_changed(&state, user.id, &id).await;
    user_data_response(&state, user.id, &id).await
}

async fn unmark_played(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, Some(false), None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, played = false, "updated Jellyfin played mark");
    websocket::publish_user_data_changed(&state, user.id, &id).await;
    user_data_response(&state, user.id, &id).await
}

async fn quickconnect_enabled() -> Json<Value> {
    Json(json!(false))
}

async fn display_preferences(Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "Id": id,
        "SortBy": "SortName",
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": "emby"
    }))
}

async fn update_display_preferences() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct ProgressBody {
    #[serde(default, rename = "ItemId")]
    item_id: Option<String>,
    #[serde(default, rename = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(default, rename = "IsPaused")]
    is_paused: Option<bool>,
}

fn client_info(headers: &HeaderMap, _body: &ProgressBody) -> PlaybackClientInfo {
    let header_value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let device_id = header_value("X-Emby-Device-Id").unwrap_or_else(|| "jellyfin-client".into());
    PlaybackClientInfo {
        device_id,
        client: header_value("X-Emby-Client"),
        device_name: header_value("X-Emby-Device-Name"),
        client_version: header_value("X-Emby-Client-Version"),
    }
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
    let client = client_info(&headers, &body);
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

async fn session_playing(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    report_session_event(state, user, body, headers, PlaybackEvent::Playing, false).await
}

async fn session_progress(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    let paused = body.is_paused.unwrap_or(false);
    report_session_event(state, user, body, headers, PlaybackEvent::Progress, paused).await
}

async fn session_stopped(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<ProgressBody>,
) -> StatusCode {
    report_session_event(state, user, body, headers, PlaybackEvent::Stopped, true).await
}
