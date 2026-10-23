use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;

use crate::http::ok_list;
use crate::management::ApiState;

/// GET /playback/favorites/gallery — 收藏图廊：每部作品的海报/背景/剧照，
/// 形状对齐前端 LibraryGalleryGroup（listFavoritesGallery 直读）。
pub(crate) async fn favorites_gallery(
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
    let is_desc = query
        .get("order")
        .map(|o| o == "desc")
        .unwrap_or_else(|| sort_key != "title");

    // Phase 1: In-lock DB reads only (media, ledger rows, favorites)
    struct MediaGalleryCandidate {
        media: domain::Media,
        library_id: String,
        preferred_coverage: Option<domain::FileCoverage>,
        owned_rows: Vec<domain::FileCoverage>,
    }

    let order = if sort_key == "title" {
        store::FavoriteOrder::Title
    } else {
        store::FavoriteOrder::FavoritedAt
    };
    let selected = {
        let store = state.store.lock();
        (|| -> Result<Vec<MediaGalleryCandidate>, store::StoreError> {
            let visible = crate::visible_scope::VisibleLibraries::load(&store, Some(user_id))?;
            let page = store.playback_favorite_page(&store::PlaybackViewQuery {
                user: user_id,
                libraries: visible.ids(),
                limit,
                offset,
                order,
                descending: is_desc,
                unwatched_first: false,
            })?;
            let mut out = Vec::with_capacity(page.items.len());
            for row in page.items {
                let Some(media) = store.get_media(row.media_id)? else {
                    continue;
                };
                let owned = visible.media_coverage(&store, row.media_id, |_| true)?;
                if owned.is_empty() {
                    continue;
                }
                out.push(MediaGalleryCandidate {
                    media,
                    library_id: row.library_id.to_string(),
                    preferred_coverage: crate::http::library::preferred_coverage(&owned).cloned(),
                    owned_rows: owned,
                });
            }
            Ok(out)
        })()
    };
    let candidates = match selected {
        Ok(candidates) => candidates,
        Err(e) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "favorites_gallery.store",
                &e.to_string(),
            );
        }
    };

    // Phase 2: Out-of-lock filesystem observation under http_work budget
    let files = state.browse_files.clone();
    let groups_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            let mut groups = Vec::new();
            for cand in candidates {
                let mut images = Vec::new();
                if let Some(ref cov) = cand.preferred_coverage {
                    let dir = cov.file.path
                        .parent()
                        .map(|p| p.to_path_buf());
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
                                    && crate::episode_still::existing_with(
                                        &cov.file.path,
                                        files.as_ref(),
                                    )
                                    .is_some()
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
        })
        .await;

    let groups = match groups_res {
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
    ok_list(groups).into_response()
}
