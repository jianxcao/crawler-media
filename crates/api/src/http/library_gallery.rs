//! GET /libraries/{id}/gallery — one image group per item, read from the `library_items`
//! summary and the published file facts. No projection, no filesystem observation.

use std::collections::HashMap;

use serde_json::{Value, json};
use axum::response::{IntoResponse, Response};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use crate::http::library::{
    browse_filter_from_query, browse_order_from_query, browse_store_error, missing_library,
    require_visible_library,
};
use crate::http::{err, ok_list};
use crate::management::ApiState;
use crate::store::library_query;

pub(crate) async fn gallery(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // 显式分页参数先校验：越界直接 400；省略 limit 仍是"返回全部"。
    let page = match crate::http::library::page::PageWindow::parse(&query) {
        Ok(page) => page,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
    }
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
        page: library_query::BrowsePage::Offset {
            start: page.offset,
            limit: page.limit,
        },
        include_total: false,
    };
    let (page_items, facts, stills) = {
        let store = state.store.lock();
        let page = match store.library_item_page(&lib_q) {
            Ok(page) => page,
            Err(error) => return browse_store_error(error),
        };
        let media_ids: Vec<domain::MediaId> = page.items.iter().map(|item| item.media_id).collect();
        let facts = match store.library_item_facts(lib_id, &media_ids, user_id) {
            Ok(facts) => facts,
            Err(error) => return browse_store_error(error),
        };
        let stills = match store.library_item_stills(lib_id, &media_ids) {
            Ok(stills) => stills,
            Err(error) => return browse_store_error(error),
        };
        (page, facts, stills)
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
        for still in stills.iter().filter(|still| still.media_id == item.media_id) {
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
    ok_list(groups).into_response()
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
