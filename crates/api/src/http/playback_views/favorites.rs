use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;

use crate::http::library;
use crate::http::ok;
use crate::management::ApiState;

/// GET /playback/favorites?limit&offset&unwatched_first&sort&order
/// — distinct favorited media from `playback_units`, level = most recent unit.
pub(crate) async fn favorites(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 200);
    let offset = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let sort_key = query
        .get("sort")
        .map(String::as_str)
        .unwrap_or("updated_at");
    // 当前端指定按 title 排序且未传 order 时，默认按 A->Z 升序排列；其他排序默认按倒序
    let is_desc = query
        .get("order")
        .map(|o| o == "desc")
        .unwrap_or_else(|| sort_key != "title");
    // 未看优先：观看分级参与排序，但不筛掉任何收藏（见 library::latest 的说明）
    let unwatched_first = query
        .get("unwatched_first")
        .is_some_and(|v| v == "true" || v == "1");
    struct FavoriteCandidate {
        media: domain::Media,
        library_id: String,
        preferred_coverage: Option<domain::FileCoverage>,
        file_count: usize,
        seasons: std::collections::BTreeSet<u32>,
        episode_count: i64,
        favorite_season_number: Option<i32>,
        favorite_episode_number: Option<i32>,
    }

    let order = match sort_key {
        "title" => store::FavoriteOrder::Title,
        "release_date" | "year" => store::FavoriteOrder::Year,
        _ => store::FavoriteOrder::FavoritedAt,
    };
    let selected = {
        let store = state.store.lock();
        (|| -> Result<_, store::StoreError> {
            let visible = crate::visible_scope::VisibleLibraries::load(&store, Some(user_id))?;
            let libraries = visible.ids();
            let page = store.playback_favorite_page(&store::PlaybackViewQuery {
                user: user_id,
                libraries,
                limit,
                offset,
                order,
                descending: is_desc,
                unwatched_first,
            })?;
            let mut cands = Vec::with_capacity(page.items.len());
            for row in page.items {
                let Some(media) = store.get_media(row.media_id)? else {
                    continue;
                };
                // Coverage spans every visible library holding the Media.
                let owned = visible.media_coverage(&store, row.media_id, |_| true)?;
                let units = || {
                    owned
                        .iter()
                        .flat_map(|cov| &cov.units)
                        .filter(|u| !u.is_whole())
                };
                cands.push(FavoriteCandidate {
                    library_id: row.library_id.to_string(),
                    preferred_coverage: library::preferred_coverage(&owned).cloned(),
                    file_count: owned.len(),
                    seasons: units().map(|u| u.season.max(0) as u32).collect(),
                    episode_count: units().count() as i64,
                    favorite_season_number: (row.unit.season >= 0).then_some(row.unit.season),
                    favorite_episode_number: (row.unit.episode >= 0).then_some(row.unit.episode),
                    media,
                });
            }
            Ok((cands, page.total))
        })()
    };
    let (candidates, total) = match selected {
        Ok(selected) => selected,
        Err(e) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "favorites.store",
                &e.to_string(),
            );
        }
    };

    let files = state.browse_files.clone();
    let page_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            let mut items = Vec::new();
            for cand in candidates {
                let poster = cand
                    .preferred_coverage
                    .as_ref()
                    .and_then(|cov| {
                        crate::http::library::poster_path_with(&cov.file, files.as_ref())
                    })
                    .map(|_| {
                        let cov = cand.preferred_coverage.as_ref().expect("checked");
                        crate::http::library::artwork_url("posters", cov.file.file_id)
                    });
                items.push(json!({
                    "media_item_id": cand.media.id.to_string(),
                    "kind": cand.media.kind.as_str(),
                    "library_id": cand.library_id,
                    "title": cand.media.title,
                    "year": cand.media.year,
                    "poster_url": poster,
                    "file_count": cand.file_count,
                    "seasons": cand.seasons,
                    "episode_count": cand.episode_count,
                    "favorite_season_number": cand.favorite_season_number,
                    "favorite_episode_number": cand.favorite_episode_number,
                }));
            }
            items
        })
        .await;

    let page = match page_res {
        Ok(items) => items,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return crate::http::err(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return crate::http::err(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "favorites.timeout",
                "收藏列表加载超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "favorites.worker",
                &msg,
            );
        }
    };
    ok(json!({ "items": page, "total": total })).into_response()
}
