//! Jellyfin/Emby catalog: items, search hints, genres and studios.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::item_dto_json;
use crate::provider::query;
use crate::provider::{QueryPrincipal, QueryTiming};

use super::parent_scope::require_user_path;
use super::{AppState, ItemsQuery, UserItemPath, UserPath, item_id_for_user};

pub(super) async fn items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(timing): Extension<QueryTiming>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Response, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        Some(QueryPrincipal {
            user_id: user.id,
            token: user.token.clone(),
            deadline: timing.deadline,
        }),
    )
    .await?;
    let libraries = state
        .provider
        .list_visible_libraries_for_query(QueryPrincipal {
            user_id: user.id,
            token: user.token.clone(),
            deadline: timing.deadline,
        })
        .await
        .map_err(|error| error.status_code())?;
    let parent = query.parent_id.as_deref();
    let recursive = query.recursive.unwrap_or(true);
    let is_folder_query = query.is_folder.unwrap_or(false);
    let is_library_parent = parent
        .map(|parent_id| libraries.iter().any(|library| library.id == parent_id))
        .unwrap_or(false);
    // 库根 + 非递归/IsFolder：仍然是 root 目录列表，保持既有协议行为。
    if is_library_parent
        && (!recursive || is_folder_query)
        && let Some(lib_id) = parent
        && let Some(target_lib) = libraries.iter().find(|library| library.id == lib_id)
    {
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
        return Ok(
            Json(json!({ "Items": folder_items, "TotalRecordCount": total })).into_response(),
        );
    }
    // 选择（过滤/排序/计数/取页）与分页重元数据都由 provider 完成：读取失败必须
    // 变成错误响应，绝不能被吞成「更小的一页」。
    let page = state
        .provider
        .query_visible_items(
            QueryPrincipal {
                user_id: user.id,
                token: user.token.clone(),
                deadline: timing.deadline,
            },
            query.to_server_query(),
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, "Jellyfin 条目选择失败");
            error.status_code()
        })?;
    let dto_started = std::time::Instant::now();
    let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
    let payload = json!({ "Items": listed, "TotalRecordCount": page.total });
    state
        .provider
        .record_query_phase("dto", dto_started.elapsed());
    let serialization_started = std::time::Instant::now();
    let response = Json(payload).into_response();
    state
        .provider
        .record_query_phase("serialization", serialization_started.elapsed());
    Ok(response)
}
pub(super) async fn resume_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        None,
    )
    .await?;
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
pub(super) async fn latest_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Extension(timing): Extension<QueryTiming>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(
        state.provider.as_ref(),
        user.id,
        path.user_id.as_deref(),
        Some(QueryPrincipal {
            user_id: user.id,
            token: user.token.clone(),
            deadline: timing.deadline,
        }),
    )
    .await?;
    // 「最近入库」的播放状态口径（见 library::latest）：显式给了 IsPlayed 就按它严格筛
    // （协议语义原样保留）；没给时**只排不筛**——没看完的在前、已看完的沉底，段内仍是
    // 入库时间倒序。Jellyfin 在缺省位置会因为 HidePlayedInLatest 直接把已看完的藏掉，
    // 全看过的库更是连入口都没有；我们改成排到最后，客户端那一行于是永远有内容。
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let principal = QueryPrincipal {
        user_id: user.id,
        token: user.token.clone(),
        deadline: timing.deadline,
    };
    let mut mapped = query.to_server_query();
    mapped.offset = start_index;
    mapped.limit = limit;
    let page_items = state
        .provider
        .query_latest_items(principal, mapped)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, "Jellyfin Latest selection failed");
            error.status_code()
        })?;
    let listed: Vec<Value> = page_items.into_iter().map(item_dto_json).collect();
    Ok(Json(json!(listed)))
}
pub(super) async fn genres(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    entity_page(state, user.id, query, crate::provider::EntityKind::Genre).await
}
async fn entity_page(
    state: AppState,
    user: domain::UserId,
    query: ItemsQuery,
    kind: crate::provider::EntityKind,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent = query::library_parent(query.parent_id.as_deref(), &libraries);
    let page = state
        .provider
        .query_entities(
            user,
            kind,
            parent,
            query.search_term.as_deref(),
            query.start_index.unwrap_or(0),
            query.limit.unwrap_or(100).clamp(1, 200),
        )
        .await
        .map_err(|error| {
            tracing::error!(%error,user_id=%user,"Jellyfin entity projection read failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let (item_type, tag) = match kind {
        crate::provider::EntityKind::Genre => ("Genre", "genre"),
        crate::provider::EntityKind::Studio => ("Studio", "studio"),
    };
    let server_id = state.provider.server_id();
    let items:Vec<_>=page.items.into_iter().map(|entity|json!({
        "Name":entity.name,"ServerId":server_id,"Id":entity.wire_id,"Type":item_type,"IsFolder":true,
        "ChildCount":entity.child_count,"ImageTags":{"Primary":tag},"BackdropImageTags":[tag],
    })).collect();
    Ok(Json(json!({"Items":items,"TotalRecordCount":page.total})))
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
    entity_page(state, user.id, query, crate::provider::EntityKind::Studio).await
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
    let page = state
        .provider
        .search_hints_page(
            user.id,
            &term,
            query.start_index.unwrap_or(0),
            query.limit.unwrap_or(20).clamp(1, 100),
        )
        .await
        .map_err(|error| {
            tracing::error!(%error,user_id=%user.id,"Jellyfin search hints failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let hints:Vec<_>=page.items.into_iter().map(|item| {
        let id=item.protocol_id();
        let item_type=if item.is_series {"Series"} else if item.media.kind==domain::MediaKind::Tv {"Episode"} else {"Movie"};
        json!({"ItemId":id,"Id":id,"Name":item.media.title,"Type":item_type,"IsFolder":item.is_series,"RunTimeTicks":item.metadata.runtime_ticks,"ProductionYear":item.metadata.production_year,"MatchedTerm":term})
    }).collect();
    Ok(Json(
        json!({"SearchHints":hints,"TotalRecordCount":page.total}),
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
