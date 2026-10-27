//! GET /libraries/{id}/gallery — one image group per item, read from the `library_items`
//! summary and the published file facts. No projection, no filesystem observation.

use std::collections::HashMap;

use crate::http::library::{
    BrowseWindow, browse_filter_from_query, browse_order_from_query, browse_response,
    missing_library,
};
use crate::http::err;
use crate::management::ApiState;
use crate::store::library_query;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub(crate) async fn gallery(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::Extension(credential): axum::Extension<store::ReadCredential>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    axum::Extension(timing): axum::Extension<media_server::provider::QueryTiming>,
) -> Response {
    // 游标分页：`cursor` 续页，`offset` 已删除；显式 limit 越界仍 400。
    let window = match BrowseWindow::parse(&query) {
        Ok(window) => window,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    let mut filter = match browse_filter_from_query(&query) {
        Ok(filter) => filter,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    // 空图组不入页：图片存在性是投影里的过滤条件，这里落在同一套 SQL 谓词上，
    // 先按「有条目图或任一单元剧照」筛，再取本页。
    filter.require_image = true;
    let (order, descending) = browse_order_from_query(&query);
    let lib_q = library_query::LibraryQuery {
        library_id: lib_id,
        user_id,
        filter,
        order,
        descending,
        page: window.page(),
        // 图廊不带总数：空图组先被剔除再分页，COUNT 口径与「有图的作品数」不同，
        // 报了反而误导；只有 `next_cursor` 决定还有没有下一页。
        include_total: false,
    };
    let selected = crate::library_read_work::run_self_library_read(
        &state,
        credential,
        lib_id,
        store::LibraryReadRequest::Browse(lib_q),
        timing.deadline,
    )
    .await;
    let (page_items, facts, stills) = match selected {
        Ok(store::LibraryReadResult::Browse {
            page,
            facts,
            stills,
        }) => (page, facts, stills),
        Ok(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "library.read",
                "查询类型错误",
            );
        }
        Err(response) => return response,
    };
    let library_id = lib_id.to_string();
    let mut groups = Vec::with_capacity(page_items.items.len());
    for item in &page_items.items {
        let facts = facts.get(&item.media_id).cloned().unwrap_or_default();
        let mut images = Vec::new();
        if let Some(file_id) = item.poster_ref {
            images.push(image_json(
                &library_id,
                &item.title,
                GalleryImageKind::Poster,
                file_id,
                None,
                None,
            ));
        }
        if let Some(file_id) = item.backdrop_ref {
            images.push(image_json(
                &library_id,
                &item.title,
                GalleryImageKind::Backdrop,
                file_id,
                None,
                None,
            ));
        }
        for still in stills
            .iter()
            .filter(|still| still.media_id == item.media_id)
        {
            images.push(image_json(
                &library_id,
                &item.title,
                GalleryImageKind::Still,
                still.file_id,
                still.season,
                still.episode,
            ));
        }
        if images.is_empty() {
            continue;
        }
        groups.push(json!({
            "library_id": library_id,
            "media_item_id": item.media_id.to_string(),
            "kind": item.kind.as_str(),
            "title": item.title,
            "year": item.year,
            "is_favorite": facts.favorite,
            "images": images,
        }));
    }
    browse_response(groups, page_items.next_cursor, None).into_response()
}

/// 图组里的一类图片。每类自带 URL 前缀、宽高比与展示名，调用方不必再传一
/// 串彼此绑定的字符串/数字参数。
#[derive(Clone, Copy)]
enum GalleryImageKind {
    Poster,
    Backdrop,
    Still,
}

impl GalleryImageKind {
    fn url_kind(self) -> &'static str {
        match self {
            Self::Poster => "posters",
            Self::Backdrop => "fanart",
            Self::Still => "stills",
        }
    }

    fn aspect(self) -> f64 {
        match self {
            Self::Poster => 0.667,
            Self::Backdrop | Self::Still => 1.778,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Poster => "poster",
            Self::Backdrop => "backdrop",
            Self::Still => "still",
        }
    }
}

fn image_json(
    library_id: &str,
    title: &str,
    kind: GalleryImageKind,
    file_id: domain::LedgerId,
    season: Option<u32>,
    episode: Option<u32>,
) -> Value {
    let label = match kind {
        GalleryImageKind::Poster => "海报".to_string(),
        GalleryImageKind::Backdrop => "背景".to_string(),
        GalleryImageKind::Still => match (season, episode) {
            (Some(s), Some(e)) => format!("第 {s} 季 第 {e} 集"),
            _ => "剧照".to_string(),
        },
    };
    json!({
        "url": crate::http::library::artwork_url(kind.url_kind(), file_id),
        "aspect": kind.aspect(),
        "kind": kind.name(),
        "alt": title,
        "library_id": library_id,
        "season": season,
        "episode": episode,
        "label": label,
        "t_seconds": Value::Null,
    })
}
