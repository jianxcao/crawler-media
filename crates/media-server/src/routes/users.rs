//! Jellyfin/Emby user profile, views, folders and display preferences.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::{library_view_json, user_profile_json};

use super::parent_scope::require_user_path;
use super::{AppState, UserPath};

#[derive(Default, Deserialize)]
pub(super) struct ViewsQuery {
    #[serde(default, rename = "IncludeHidden", alias = "includeHidden")]
    include_hidden: Option<bool>,
}
pub(super) async fn user_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        None,
    )
    .await?;
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

pub(super) async fn user_views(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ViewsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        None,
    )
    .await?;
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
pub(super) async fn media_folders(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let server_id = state.provider.server_id();
    let views: Vec<Value> = libraries
        .iter()
        .map(|lib| {
            let mut val = library_view_json(lib, &server_id);
            val["Type"] = json!("Folder");
            val["IsFolder"] = json!(true);
            val
        })
        .collect();
    let total = views.len();
    Ok(Json(json!({
        "Items": views,
        "TotalRecordCount": total,
    })))
}

pub(super) async fn virtual_folders(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let views: Vec<Value> = libraries
        .iter()
        .map(|lib| {
            json!({
                "Name": lib.name,
                "ItemId": lib.id,
                "Id": lib.id,
                "Locations": lib.root_paths,
                "CollectionType": match lib.kind {
                    domain::MediaKind::Movie => "movies",
                    domain::MediaKind::Tv => "tvshows",
                    domain::MediaKind::Video => "homevideos",
                },
            })
        })
        .collect();
    Ok(Json(json!(views)))
}

pub(super) async fn item_counts(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        None,
    )
    .await?;
    let counts = state
        .provider
        .query_item_counts(user.id)
        .await
        .map_err(|error| {
            tracing::error!(%error,user_id=%user.id,"Jellyfin item counts failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(
        json!({"MovieCount":counts.movies,"SeriesCount":counts.series,"EpisodeCount":counts.episodes,"ItemCount":counts.items}),
    ))
}
pub(super) async fn display_preferences(Path(id): Path<String>) -> Json<Value> {
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

pub(super) async fn update_display_preferences() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
