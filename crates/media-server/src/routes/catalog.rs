//! Jellyfin/Emby catalog: items, search hints, genres and studios.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::item_dto_json;
use crate::provider::ServerItemPage;
use crate::provider::query;

use super::parent_scope::require_user_path;
use super::{AppState, ItemsQuery, UserItemPath, UserPath, item_id_for_user};

pub(super) async fn items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent = query.parent_id.as_deref();
    let recursive = query.recursive.unwrap_or(true);
    let is_folder_query = query.is_folder.unwrap_or(false);
    let is_library_parent = parent
        .map(|parent_id| libraries.iter().any(|library| library.id == parent_id))
        .unwrap_or(false);
    // 库根 + 非递归/IsFolder：仍然是 root 目录列表，保持既有协议行为。
    if is_library_parent && (!recursive || is_folder_query) {
        if let Some(lib_id) = parent {
            if let Some(target_lib) = libraries.iter().find(|library| library.id == lib_id) {
                let mut folder_items: Vec<Value> = Vec::new();
                let server_id = state.provider.server_id();
                for root in &target_lib.root_paths {
                    let folder_name = root
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("folder");
                    let folder_id = crate::provider::query::folder_id_for_root(root);
                    folder_items.push(json!({
                        "Name": folder_name,
                        "ServerId": server_id,
                        "Id": folder_id,
                        "Type": "Folder",
                        "IsFolder": true,
                        "CollectionType": match target_lib.kind {
                            domain::MediaKind::Movie => "movies",
                            domain::MediaKind::Tv => "tvshows",
                            domain::MediaKind::Video => "homevideos",
                        },
                    }));
                }
                let total = folder_items.len();
                return Ok(Json(
                    json!({ "Items": folder_items, "TotalRecordCount": total }),
                ));
            }
        }
    }
    // 选择（过滤/排序/计数/取页）与分页重元数据都由 provider 完成：读取失败必须
    // 变成错误响应，绝不能被吞成「更小的一页」。
    let page = state
        .provider
        .query_visible_items(user.id, query.to_server_query())
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, "Jellyfin 条目选择失败");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
    Ok(Json(
        json!({ "Items": listed, "TotalRecordCount": page.total }),
    ))
}
pub(super) async fn resume_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let start_index = query.start_index.unwrap_or(0);
    if query.parent_id.is_none() {
        // 无 ParentId：选页在 store 侧完成，只为返回页建重元数据。
        let page = state
            .provider
            .resumable_items_page(user.id, start_index, limit)
            .await
            .map_err(|error| {
                tracing::error!(%error, user_id = %user.id, "Jellyfin 续播选页失败");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
        return Ok(Json(
            json!({ "Items": listed, "TotalRecordCount": page.total }),
        ));
    }
    let mut rows = {
        let root_rows = state
            .provider
            .list_visible_items(user.id, query.parent_id.as_deref())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        root_rows
            .into_iter()
            .filter(|item| item.ticks > 0 && !item.played)
            .collect::<Vec<_>>()
    };
    let total = rows.len();
    let listed: Vec<Value> = rows
        .drain(..)
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
/// `/Items/Latest` 的一段：谓词、排序、COUNT、LIMIT/OFFSET 都在 store 的同一次短读里完成，
/// 调用方拿到的 `items` 就是最终返回的那几行（只为它们建重元数据）。
async fn latest_segment(
    state: &AppState,
    user_id: domain::UserId,
    query: &ItemsQuery,
    played: bool,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, StatusCode> {
    let mut mapped = query.to_server_query();
    mapped.is_played = Some(played);
    mapped.sort_by = Some("DateCreated,SortName".to_string());
    mapped.sort_order = Some("Descending".to_string());
    mapped.offset = offset;
    mapped.limit = limit;
    state
        .provider
        .query_visible_items(user_id, mapped)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user_id, "Jellyfin 最近入库选页失败");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub(super) async fn latest_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    // 「最近入库」的播放状态口径（见 library::latest）：显式给了 IsPlayed 就按它严格筛
    // （协议语义原样保留）；没给时**只排不筛**——没看完的在前、已看完的沉底，段内仍是
    // 入库时间倒序。Jellyfin 在缺省位置会因为 HidePlayedInLatest 直接把已看完的藏掉，
    // 全看过的库更是连入口都没有；我们改成排到最后，客户端那一行于是永远有内容。
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let mut page_items = Vec::new();
    match query.is_played {
        Some(played) => {
            let page = latest_segment(&state, user.id, &query, played, start_index, limit).await?;
            page_items = page.items;
        }
        None => {
            // 未看完段先选页；只有它装不满本页时才去取已看完段。
            let unplayed =
                latest_segment(&state, user.id, &query, false, start_index, limit).await?;
            let unplayed_total = unplayed.total;
            page_items = unplayed.items;
            if page_items.len() < limit {
                // 整段顺序 = 未看完[0..U) ++ 已看完[0..P)：越过 U 之后，
                // 改从已看完段的第 (start + 已取条数 - U) 条接上。
                let taken = page_items.len();
                let played_offset = (start_index + taken).saturating_sub(unplayed_total);
                let played_page =
                    latest_segment(&state, user.id, &query, true, played_offset, limit - taken)
                        .await?;
                page_items.extend(played_page.items);
            }
        }
    }
    let listed: Vec<Value> = page_items.into_iter().map(item_dto_json).collect();
    Ok(Json(json!(listed)))
}
pub(super) async fn genres(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent_id = query::library_parent(query.parent_id.as_deref(), &libraries);
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut genre_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for it in items {
        for g in it.metadata.genres {
            if !g.trim().is_empty() {
                *genre_counts.entry(g).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in genre_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Genre",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "genre" },
            "BackdropImageTags": ["genre"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn genre_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Genre",
        "IsFolder": true,
        "ImageTags": { "Primary": "genre" },
        "BackdropImageTags": ["genre"],
    })))
}
pub(super) async fn studios(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent_id = query::library_parent(query.parent_id.as_deref(), &libraries);
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut studio_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for it in items {
        for s in it.metadata.studios {
            if !s.trim().is_empty() {
                *studio_counts.entry(s).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in studio_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Studio",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "studio" },
            "BackdropImageTags": ["studio"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn studio_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Studio",
        "IsFolder": true,
        "ImageTags": { "Primary": "studio" },
        "BackdropImageTags": ["studio"],
    })))
}
#[derive(Default, Deserialize)]
pub(super) struct SearchHintsQuery {
    #[serde(default, rename = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(default, rename = "StartIndex", alias = "startIndex")]
    start_index: Option<usize>,
    #[serde(default, rename = "Limit", alias = "limit")]
    limit: Option<usize>,
}
pub(super) async fn search_hints(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<SearchHintsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let Some(term) = query
        .search_term
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
    else {
        return Ok(Json(json!({ "SearchHints": [], "TotalRecordCount": 0 })));
    };
    let items = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut hints = Vec::new();
    for it in items {
        let title_match = it.media.title.to_lowercase().contains(&term);
        let orig_match = it
            .metadata
            .original_title
            .as_deref()
            .map(|s| s.to_lowercase().contains(&term))
            .unwrap_or(false);
        if title_match || orig_match {
            let id = it.protocol_id();
            let item_type = if it.is_series {
                "Series"
            } else if it.media.kind == domain::MediaKind::Tv {
                "Episode"
            } else {
                "Movie"
            };
            hints.push(json!({
                "ItemId": id,
                "Id": id,
                "Name": it.media.title,
                "Type": item_type,
                "IsFolder": it.is_series,
                "RunTimeTicks": it.metadata.runtime_ticks,
                "ProductionYear": it.metadata.production_year,
                "MatchedTerm": term,
            }));
        }
    }
    let total = hints.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = hints.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(
        json!({ "SearchHints": listed, "TotalRecordCount": total }),
    ))
}
pub(super) async fn item_by_id(
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
