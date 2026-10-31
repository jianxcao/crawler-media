use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;

use crate::http::ok;
use crate::management::ApiState;

struct MediaGalleryCandidate {
    media: domain::Media,
    library_id: String,
    preferred_coverage: Option<domain::FileCoverage>,
    owned_rows: Vec<domain::FileCoverage>,
}

/// GET /playback/favorites/gallery — 收藏图廊：每部作品的海报/背景/剧照，
/// 形状对齐前端 LibraryGalleryGroup（listFavoritesGallery 直读）。
pub(crate) async fn favorites_gallery(
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
        candidates.push(MediaGalleryCandidate {
            media,
            library_id: row.library_id.to_string(),
            preferred_coverage: crate::http::library::preferred_coverage(&owned).cloned(),
            owned_rows: owned,
        });
    }

    // Phase 2: Out-of-lock filesystem observation under http_work budget
    let files = state.file_stats.clone();
    let groups_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            render_gallery(candidates, files.as_ref())
        })
        .await;

    let response = gallery_response(groups_res, total, next_cursor);
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

fn render_gallery(
    candidates: Vec<MediaGalleryCandidate>,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Vec<serde_json::Value> {
    let mut groups = Vec::new();
    for cand in candidates {
        let mut images = Vec::new();
        if let Some(ref cov) = cand.preferred_coverage {
            let dir = cov.file.path.parent().map(|p| p.to_path_buf());
            if let Some(dir) = dir {
                if files.is_file(&dir.join("poster.jpg")) {
                    images.push(json!({
                        "kind": "poster",
                        "url": crate::http::library::artwork_url("posters", cov.file.file_id),
                        "aspect": 0.667,
                        "label": "海报",
                        "season": null,
                        "episode": null,
                        "t_seconds": null,
                    }));
                }
                if files.is_file(&dir.join("fanart.jpg")) {
                    images.push(json!({
                        "kind": "backdrop",
                        "url": crate::http::library::artwork_url("fanart", cov.file.file_id),
                        "aspect": 1.778,
                        "label": "背景",
                        "season": null,
                        "episode": null,
                        "t_seconds": null,
                    }));
                }
                for cov in &cand.owned_rows {
                    for unit in &cov.units {
                        if !unit.is_whole()
                            && crate::episode_still::existing_with(&cov.file.path, files).is_some()
                        {
                            images.push(json!({
                                "kind": "still",
                                "url": crate::http::library::artwork_url("stills", cov.file.file_id),
                                "aspect": 1.778,
                                "label": "剧照",
                                "season": unit.season,
                                "episode": unit.episode,
                                "t_seconds": null,
                            }));
                        }
                    }
                }
            }
        }
        groups.push(json!({
            "media_item_id": cand.media.id.to_string(),
            "library_id": cand.library_id,
            "kind": cand.media.kind.as_str(),
            "title": cand.media.title,
            "year": cand.media.year,
            "is_favorite": true,
            "images": images,
        }));
    }
    groups
}

fn gallery_response(
    result: Result<Vec<serde_json::Value>, crate::blocking_budget::BlockingError>,
    total: Option<usize>,
    next_cursor: Option<String>,
) -> Response {
    let groups = match result {
        Ok(groups) => groups,
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
                "gallery.timeout",
                "收藏图廊加载超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "gallery.worker",
                &msg,
            );
        }
    };
    ok(json!({"items": groups, "total": total, "next_cursor": next_cursor})).into_response()
}
