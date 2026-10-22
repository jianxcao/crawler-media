//! Jellyfin/Emby shows: NextUp, seasons and episodes.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::item_dto_json;

use super::{AppState, ItemsQuery};

pub(super) async fn shows_next_up(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.start_index.unwrap_or(0);
    // 一次聚合：候选按 Media 分组、每个 Media 取第一条未看，再整体分页。读取失败
    // 必须变成错误响应，绝不能被吞成「空的一页」。
    let page = state
        .provider
        .next_up_items(user.id, query.series_id.as_deref(), offset, limit)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, "Jellyfin NextUp 聚合失败");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
    Ok(Json(
        json!({ "Items": listed, "TotalRecordCount": page.total }),
    ))
}
pub(super) async fn show_seasons(
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

    let series_id = domain::TypedItemId::Work(item.media.id).to_wire_string();
    let episodes = state
        .provider
        .list_visible_items(user.id, Some(&series_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut season_numbers: std::collections::BTreeSet<u32> = episodes
        .iter()
        .map(|episode| episode.unit.map(|u| u.season as u32).unwrap_or(1))
        .collect();
    if season_numbers.is_empty() {
        season_numbers.insert(item.unit.map(|u| u.season as u32).unwrap_or(1));
    }
    let server_id = state.provider.server_id();
    let mut seasons_json: Vec<Value> = Vec::new();
    for season_num in season_numbers {
        let season_id = domain::TypedItemId::Season {
            media: item.media.id,
            season: season_num,
        }
        .to_wire_string();
        let season_episodes: Vec<&crate::provider::MediaItemSnapshot> = episodes
            .iter()
            .filter(|e| e.unit.map(|u| u.season as u32) == Some(season_num))
            .collect();
        let played_count = season_episodes.iter().filter(|e| e.played).count();
        let season_total = season_episodes.len();
        let unplayed = season_total.saturating_sub(played_count);
        let user_data = json!({
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": season_total > 0 && unplayed == 0,
            "UnplayedItemCount": unplayed,
            "PlayedPercentage": if season_total > 0 { played_count as f64 / season_total as f64 * 100.0 } else { 0.0 },
        });
        let primary_tag = item.metadata.primary_image_tag.clone();
        seasons_json.push(json!({
            "Id": season_id,
            "Name": format!("第 {} 季", season_num),
            "SeriesId": series_id,
            "SeriesName": item.media.title,
            "IndexNumber": season_num,
            "Type": "Season",
            "IsFolder": true,
            "ServerId": server_id,
            "UserData": user_data,
            "SeriesPrimaryImageTag": primary_tag,
            "ImageTags": {
                "Primary": primary_tag.unwrap_or_else(|| "poster".to_string())
            },
            "BackdropImageTags": [],
        }));
    }
    Ok(Json(
        json!({ "Items": seasons_json, "TotalRecordCount": seasons_json.len() }),
    ))
}
pub(super) async fn show_episodes(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    // 授权、剧/季收敛、排序、COUNT 与 LIMIT 都在 store 的同一段短读里完成；这里
    // 只把返回页渲染成 JSON。旧实现先整库取快照、再在内存里过滤排序切页。
    let page = state
        .provider
        .query_visible_items(user.id, episode_query(&id, &query))
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, series_id = %id, "Jellyfin 分集选页失败");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
    Ok(Json(
        json!({ "Items": listed, "TotalRecordCount": page.total }),
    ))
}

/// 分集请求 → 协议查询：钉住这部剧，只列分集，按 (季, 集) 升序。
/// 旧实现在这条路由上忽略 `PersonIds`/`Filters`/`Genres` 等契约字段，这里显式
/// 清空，绝不因为它们把结果收窄成旧行为之外的一页。
fn episode_query(series_id: &str, query: &ItemsQuery) -> crate::provider::ServerItemQuery {
    let mut mapped = query.to_server_query();
    mapped.parent_id = Some(series_id.to_string());
    mapped.series_id = None;
    mapped.include_item_types = Some("Episode".to_string());
    mapped.recursive = true;
    mapped.person_ids = None;
    mapped.filters = None;
    mapped.is_favorite = None;
    mapped.is_played = None;
    mapped.genres = None;
    mapped.genre_ids = None;
    mapped.studio_ids = None;
    mapped.search_term = None;
    mapped.is_folder = None;
    mapped.sort_by = Some("ParentIndexNumber,IndexNumber".to_string());
    mapped.sort_order = Some("Ascending".to_string());
    mapped
}
