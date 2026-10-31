use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;

use crate::http::library;
use crate::http::ok;
use crate::management::ApiState;

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

/// GET /playback/favorites?limit&after&unwatched_first&sort&order
/// — distinct favorited media from `playback_units`, level = most recent unit.
pub(crate) async fn favorites(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let query = match super::favorite_read::query(&query, user_id) {
        Ok(query) => query,
        Err(response) => return response,
    };
    for _ in 0..2 {
        match page(&state, user_id, query.clone()).await {
            Ok((response, true)) => return response,
            Ok((_, false)) => continue,
            Err(response) => return response,
        }
    }
    super::favorite_read::changed()
}

async fn page(
    state: &ApiState,
    user_id: domain::UserId,
    query: store::PlaybackViewQuery,
) -> Result<(Response, bool), Response> {
    let selection = match crate::http::store_wait::store_read(state, move |store| {
        super::favorite_read::capture(store, query)
    })
    .await
    {
        Ok(Ok(selection)) => selection,
        Ok(Err(error)) => return Err(super::favorite_read::store_error(error)),
        Err(error) => return Err(error.response()),
    };
    let snapshot = selection.snapshot(selection.libraries.clone());
    let total = selection.page.total;
    let next_cursor = selection.page.next_cursor;
    let mut candidates = Vec::with_capacity(selection.page.items.len());
    let mut media = selection.media;
    let mut coverages = selection.coverage;
    for row in selection.page.items {
        let Some(media) = media.remove(&row.media_id) else {
            continue;
        };
        let owned = coverages.remove(&row.media_id).unwrap_or_default();
        let units = || {
            owned
                .iter()
                .flat_map(|coverage| &coverage.units)
                .filter(|unit| !unit.is_whole())
        };
        candidates.push(FavoriteCandidate {
            library_id: row.library_id.to_string(),
            preferred_coverage: library::preferred_coverage(&owned).cloned(),
            file_count: owned.len(),
            seasons: units().map(|unit| unit.season.max(0) as u32).collect(),
            episode_count: units().count() as i64,
            favorite_season_number: (row.unit.season >= 0).then_some(row.unit.season),
            favorite_episode_number: (row.unit.episode >= 0).then_some(row.unit.episode),
            media,
        });
    }

    let files = state.file_stats.clone();
    let page_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            render_favorites(candidates, files.as_ref())
        })
        .await;

    let response = favorites_response(page_res, total, next_cursor);
    let valid = match crate::http::store_wait::store_read(state, move |store| {
        super::favorite_read::validate(store, user_id, &snapshot)
    })
    .await
    {
        Ok(Ok(valid)) => valid,
        Ok(Err(error)) => return Err(super::favorite_read::store_error(error)),
        Err(error) => return Err(error.response()),
    };
    Ok((response, valid))
}

fn render_favorites(
    candidates: Vec<FavoriteCandidate>,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for cand in candidates {
        let poster = cand
            .preferred_coverage
            .as_ref()
            .filter(|cov| crate::http::library::poster_path_with(&cov.file, files).is_some())
            .map(|cov| crate::http::library::artwork_url("posters", cov.file.file_id));
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
}

fn favorites_response(
    result: Result<Vec<serde_json::Value>, crate::blocking_budget::BlockingError>,
    total: Option<usize>,
    next_cursor: Option<String>,
) -> Response {
    let page = match result {
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
    ok(json!({ "items": page, "total": total, "next_cursor": next_cursor })).into_response()
}
