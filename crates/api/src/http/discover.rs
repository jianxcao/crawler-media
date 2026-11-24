//! Discover: catalog rows for movie / TV walls, optionally one source.
//! Section table mirrors the upstream discover page (20 movie + 20 TV rows).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use media::CatalogHit;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::blocking_budget::BlockingError;
use crate::catalog::Catalog;
use crate::discover_refresh::{
    resolve_source, select_catalog, spawn_swr_closure_refresh, spawn_swr_sections_refresh,
};
use crate::http::{err, ok};
use crate::management::ApiState;

mod executor;
mod sections;
pub(crate) use executor::Executor;
pub(crate) use sections::{
    Section, douban_movie_sections, douban_tv_sections, movie_sections, tv_sections,
};

use self::sections::{collection_title, section_query, supports_full_listing};

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";
const TMDB_BACKDROP: &str = "https://image.tmdb.org/t/p/w1280";
/// 只读 catalog 墙的总等待预算；不替代 catalog 自身的 HTTP 超时。
const DISCOVER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Runtime failures of the blocking budget (not upstream catalog errors).
fn blocking_error(error: BlockingError) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "discover.timeout",
            "分区加载超时",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "discover.worker",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct DiscoverQuery {
    /// "tmdb" | "douban" | absent = all sources merged.
    source: Option<String>,
}

pub(crate) async fn discover_kind(
    State(state): State<ApiState>,
    viewer: Option<axum::Extension<Option<domain::UserId>>>,
    Path(kind): Path<String>,
    Query(query): Query<DiscoverQuery>,
) -> Response {
    let viewer = viewer.and_then(|ext| ext.0);
    let catalog = state.catalog.as_ref();
    let requested_source = query.source.filter(|source| !source.is_empty());
    let catalog = match requested_source.as_deref() {
        Some(source) => match resolve_source(catalog, source) {
            Some(found) => found,
            None => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "discover.upstream",
                    &format!("未知数据源: {source}"),
                );
            }
        },
        None => catalog,
    };
    let sections = match (kind.as_str(), catalog.source_name()) {
        ("movie", "douban") => douban_movie_sections(),
        ("tv", "douban") => douban_tv_sections(),
        ("movie", _) => movie_sections(),
        ("tv", _) => tv_sections(),
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "discover.upstream",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let (sections, stale_sections) = match state
        .discover_work
        .run(state.catalog.clone(), requested_source.clone(), sections)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => return blocking_error(error),
    };
    if !stale_sections.is_empty() {
        let flight_key = format!(
            "wall:{}:{kind}",
            requested_source.as_deref().unwrap_or("fanout")
        );
        spawn_swr_sections_refresh(&state, flight_key, requested_source, stale_sections);
    }

    let enriched = super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App],
        move |store| {
            let tmdb_configured = store
                .get_setting(crate::settings_keys::TMDB_API_KEY)
                .ok()
                .flatten()
                .is_some_and(|value| !value.is_empty());
            let mut sections = sections;
            let candidates = collect_section_alias_keys(&sections);
            let owned = owned_aliases_in_visible_libraries(store, viewer, &candidates)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "发现墙批量入库状态查询失败，降级为无标记");
                    HashMap::new()
                });
            apply_ownership_to_sections(&mut sections, &owned);
            (tmdb_configured, sections)
        },
    )
    .await;
    let (tmdb_configured, sections) = match enriched {
        Ok(result) => result,
        Err(error) => return error.response(),
    };
    ok(json!({
        "kind": kind,
        "tmdb_configured": tmdb_configured,
        "sections": sections,
    }))
    .into_response()
}

/// One wall row: (kind, id, title, presentation, items).
fn row(
    source: &str,
    kind: &str,
    id: &str,
    title: &str,
    presentation: &str,
    result: Result<Vec<CatalogHit>, String>,
) -> Value {
    match result {
        Ok(hits) => json!({
            "id": id,
            "title": title,
            "presentation": presentation,
            "supports_full_listing": supports_full_listing(source, kind, id),
            "items": hits.into_iter().map(item_json).collect::<Vec<_>>(),
        }),
        Err(error) => {
            tracing::warn!(source, kind, section = id, %error, "发现墙单分区加载失败，优雅降级");
            json!({
                "id": id,
                "title": title,
                "presentation": presentation,
                "supports_full_listing": supports_full_listing(source, kind, id),
                "items": [],
                "error": {
                    "code": "discover.section_failed",
                    "message": "分区暂时无法加载，请重试"
                },
            })
        }
    }
}

fn item_json(hit: CatalogHit) -> Value {
    let id = hit
        .media
        .tmdb_id
        .clone()
        .or(hit.media.douban_id.clone())
        .unwrap_or_else(|| hit.media.id.to_string());
    let mut obj = json!({
        "id": id,
        "title": hit.media.title,
        "year": hit.media.year,
        "kind": hit.media.kind.as_str(),
        "tmdb_id": hit.media.tmdb_id,
        "poster_url": poster_url(hit.poster_path.as_deref()),
        "backdrop_url": backdrop_url(hit.backdrop_path.as_deref()),
        "rating": hit.rating,
        "overview": hit.overview,
    });
    if let Some(douban_id) = hit.media.douban_id
        && let Some(map) = obj.as_object_mut()
    {
        map.insert("douban_id".into(), Value::String(douban_id));
    }
    obj
}

fn poster_url(path: Option<&str>) -> Option<String> {
    remote_or_prefixed(path, TMDB_POSTER)
}

fn backdrop_url(path: Option<&str>) -> Option<String> {
    remote_or_prefixed(path, TMDB_BACKDROP)
}

fn remote_or_prefixed(path: Option<&str>, prefix: &str) -> Option<String> {
    let path = path.filter(|path| !path.is_empty())?;
    if path.starts_with("http://") || path.starts_with("https://") {
        Some(path.to_string())
    } else if path.starts_with('/') {
        Some(format!("{prefix}{path}"))
    } else {
        None
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct FilterQuery {
    source: Option<String>,
    genres: Option<String>,
    country: Option<String>,
    year: Option<u32>,
    rating: Option<f32>,
    runtime: Option<u32>,
    sort: Option<String>,
    page: Option<i64>,
}

fn encode_filter_value(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b'|' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn build_filter_query_string(kind: &str, query: &FilterQuery) -> String {
    let mut params = Vec::new();
    if let Some(genres) = query.genres.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("with_genres={}", encode_filter_value(genres)));
    }
    // TV 且用户没指定类型时，默认排除综艺/真人秀（TMDB 的 CN origin 大量是
    // 这些，用户筛「中国剧」时几乎总是想要剧情剧而不是《奔跑吧》）。
    if kind == "tv" && query.genres.as_deref().unwrap_or("").is_empty() {
        params.push("without_genres=10764,10767".to_string());
    }
    if let Some(country) = query.country.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!(
            "with_origin_country={}",
            encode_filter_value(country)
        ));
    }
    let is_tv = kind == "tv";
    let sort = match query.sort.as_deref() {
        Some("rating") => "vote_average.desc",
        // TV 的「最新」按首播日，电影按上映日；用错字段 TMDB 会忽略条件。
        Some("newest") => {
            if is_tv {
                "first_air_date.desc"
            } else {
                "primary_release_date.desc"
            }
        }
        Some("most-rated") => "vote_count.desc",
        _ => "popularity.desc",
    };
    params.push(format!("sort_by={sort}"));
    if let Some(year) = query.year {
        if is_tv {
            params.push(format!("first_air_date_year={year}"));
        } else {
            params.push(format!("primary_release_year={year}"));
        }
    }
    if let Some(rating) = query.rating {
        params.push(format!("vote_average.gte={rating}"));
    }
    if let Some(runtime) = query.runtime {
        params.push(format!("with_runtime.lte={runtime}"));
    }
    params.join("&")
}

fn fetch_filtered_page(
    catalog: &dyn Catalog,
    kind: &str,
    query_string: &str,
    page: i64,
) -> Result<(Vec<CatalogHit>, i64, i64), (u16, String)> {
    match kind {
        "movie" => match catalog.collection_paged(domain::MediaKind::Movie, query_string, page) {
            Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
            _ => {
                let hits = catalog.filtered_movie(query_string).map_err(|e| (502, e))?;
                let len = hits.len() as i64;
                Ok((hits, if len > 0 { 1 } else { 0 }, len))
            }
        },
        "tv" => match catalog.collection_paged(domain::MediaKind::Tv, query_string, page) {
            Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
            _ => {
                let hits = catalog.filtered_tv(query_string).map_err(|e| (502, e))?;
                let len = hits.len() as i64;
                Ok((hits, if len > 0 { 1 } else { 0 }, len))
            }
        },
        _ => Err((400, "kind 必须是 movie 或 tv".into())),
    }
}

/// Filtered wall: `GET /discover/{kind}/filtered?source=..&genres=..&country=..
/// &year=..&rating=..&runtime=..&sort=..`.
pub(crate) async fn discover_filtered(
    State(state): State<ApiState>,
    viewer: Option<axum::Extension<Option<domain::UserId>>>,
    Path(kind): Path<String>,
    Query(query): Query<FilterQuery>,
) -> Response {
    let viewer = viewer.and_then(|ext| ext.0);
    let requested_source = query
        .source
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(source) = requested_source.as_deref()
        && resolve_source(state.catalog.as_ref(), source).is_none()
    {
        return err(StatusCode::BAD_REQUEST, "discover.source", "未知数据源");
    }
    let query_string = build_filter_query_string(&kind, &query);
    let page = query.page.unwrap_or(1).max(1);
    let state_clone = state.clone();
    let kind_clone = kind.clone();
    let qs_clone = query_string.clone();
    let source_clone = requested_source.clone();
    let paged_res = state
        .http_work
        .run(
            DISCOVER_DEADLINE,
            move || -> Result<((Vec<CatalogHit>, i64, i64), bool), (u16, String)> {
                let swr = media::cache::with_prefer_stale(|| {
                    let catalog =
                        select_catalog(state_clone.catalog.as_ref(), source_clone.as_deref());
                    fetch_filtered_page(catalog, &kind_clone, &qs_clone, page)
                });
                let stale = swr.used_stale();
                swr.value.map(|res| (res, stale))
            },
        )
        .await;

    let ((hits, total_pages, total_results), used_stale) = match paged_res {
        Ok(Ok(res)) => res,
        Ok(Err((status, message))) => {
            return err(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                "discover.upstream",
                &message,
            );
        }
        Err(error) => return blocking_error(error),
    };
    if used_stale {
        let flight_key = format!(
            "filtered:{}:{kind}:{query_string}:p{page}",
            requested_source.as_deref().unwrap_or("fanout")
        );
        let kind_bg = kind.clone();
        let qs_bg = query_string.clone();
        let src_bg = requested_source;
        spawn_swr_closure_refresh(&state, flight_key, move |root| {
            let catalog = select_catalog(root, src_bg.as_deref());
            if let Err((status, error)) = fetch_filtered_page(catalog, &kind_bg, &qs_bg, page) {
                tracing::warn!(kind = %kind_bg, query = %qs_bg, page, status, %error, "发现筛选页后台 SWR 刷新失败");
            }
        });
    }

    let mut items: Vec<Value> = hits.into_iter().map(item_json).collect();
    if let Err(response) = enrich_items_ownership(&state, viewer, &mut items).await {
        return response;
    }
    ok(json!({
        "kind": kind,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
        "has_more": page < total_pages,
        "items": items,
    }))
    .into_response()
}

/// `GET /discover/{kind}/collection/{id}?source=&page=1&page_size=20` — a full
/// listing for one wall section (分页). 404 when the section has no listing.
/// source=douban 走豆瓣翻页（仅可翻页分区有入口），否则 TMDB discover。
pub(crate) async fn discover_collection(
    State(state): State<ApiState>,
    viewer: Option<axum::Extension<Option<domain::UserId>>>,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<CollectionQuery>,
) -> Response {
    let viewer = viewer.and_then(|ext| ext.0);
    let media_kind = match kind.as_str() {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "discover.kind",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let mut catalog: &dyn Catalog = state.catalog.as_ref();
    let requested_source = query
        .source
        .as_deref()
        .filter(|source| !source.is_empty())
        .map(str::to_string);
    if let Some(source) = requested_source.as_deref() {
        match resolve_source(catalog, source) {
            Some(found) => catalog = found,
            None => return err(StatusCode::BAD_REQUEST, "discover.source", "未知数据源"),
        }
    }
    let is_douban = catalog.source_name() == "douban";
    let normalized_id = match id.as_str() {
        "movie_high_score" | "movie_top250" => "top-rated",
        other => other,
    };
    if is_douban && normalized_id != "top-rated" {
        return err(
            StatusCode::NOT_FOUND,
            "discover.collection",
            "该分区不支持豆瓣完整榜单",
        );
    }
    let Some(section_query) = section_query(&kind, normalized_id) else {
        return err(
            StatusCode::NOT_FOUND,
            "discover.collection",
            "该分区不支持完整榜单",
        );
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(10, 100);
    let state_clone = state.clone();
    let source_clone = requested_source.clone();
    let sq_clone = section_query.clone();
    let paged = state
        .http_work
        .run(DISCOVER_DEADLINE, move || {
            let swr = media::cache::with_prefer_stale(|| {
                let selected =
                    select_catalog(state_clone.catalog.as_ref(), source_clone.as_deref());
                selected.collection_paged(media_kind, &sq_clone, page)
            });
            let stale = swr.used_stale();
            (swr.value, stale)
        })
        .await;
    let (paged, used_stale) = match paged {
        Ok((Ok(Some(result)), stale)) => (result, stale),
        Ok((Ok(None), _)) => {
            return err(
                StatusCode::NOT_FOUND,
                "discover.collection",
                "该分区不支持完整榜单",
            );
        }
        Ok((Err(error), _)) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
        Err(error) => return blocking_error(error),
    };
    if used_stale {
        let flight_key = format!(
            "collection:{}:{kind}:{normalized_id}:p{page}",
            requested_source.as_deref().unwrap_or("fanout")
        );
        let kind_bg = kind.clone();
        let id_bg = normalized_id.to_string();
        spawn_swr_closure_refresh(&state, flight_key, move |root| {
            let selected = select_catalog(root, requested_source.as_deref());
            if let Err(error) = selected.collection_paged(media_kind, &section_query, page) {
                tracing::warn!(kind = %kind_bg, collection_id = %id_bg, page, %error, "发现榜单页后台 SWR 刷新失败");
            }
        });
    }

    let (hits, _, total_pages, total_results) = paged;
    let mut items: Vec<Value> = hits
        .into_iter()
        .take(page_size as usize)
        .map(item_json)
        .collect();
    if let Err(response) = enrich_items_ownership(&state, viewer, &mut items).await {
        return response;
    }
    ok(json!({
        "kind": kind,
        "collection_id": normalized_id,
        "title": collection_title(&kind, normalized_id),
        "items": items,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
        "has_more": page < total_pages,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(crate) struct CollectionQuery {
    source: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

/// `GET /genres/{kind}` — filter dialog needs the TMDB genre list.
pub(crate) async fn genres(State(state): State<ApiState>, Path(kind): Path<String>) -> Response {
    let media_kind = match kind.as_str() {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "discover.kind",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let state_clone = state.clone();
    let rows = state
        .http_work
        .run(DISCOVER_DEADLINE, move || {
            state_clone.catalog.genres(media_kind)
        })
        .await;
    let rows = match rows {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
        Err(error) => return blocking_error(error),
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ok(json!({ "kind": kind, "genres": items })).into_response()
}

type AliasKey = (String, String, String);

#[derive(Clone)]
struct OwnedSummary {
    media_item_id: String,
    library_count: i64,
    file_count: i64,
}

fn collect_item_alias_keys(items: &[Value], out: &mut Vec<AliasKey>) {
    for item in items {
        let Some(kind) = item.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if let Some(tmdb_id) = item
            .get("tmdb_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            out.push(("tmdb".into(), kind.into(), tmdb_id.into()));
        }
        if let Some(douban_id) = item
            .get("douban_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            out.push(("douban".into(), kind.into(), douban_id.into()));
        }
    }
}

fn collect_section_alias_keys(sections: &[Value]) -> Vec<AliasKey> {
    let mut keys = Vec::new();
    for section in sections {
        if let Some(items) = section.get("items").and_then(Value::as_array) {
            collect_item_alias_keys(items, &mut keys);
        }
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn owned_aliases_in_visible_libraries(
    store: &crate::Store,
    viewer: Option<domain::UserId>,
    candidates: &[AliasKey],
) -> Result<HashMap<AliasKey, OwnedSummary>, store::StoreError> {
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    let visible_libs: Vec<String> = crate::visible_scope::VisibleLibraries::load(store, viewer)?
        .ids()
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    if visible_libs.is_empty() {
        return Ok(HashMap::new());
    }
    store.with_read(store::Db::Library, |conn| {
        let mut owned = HashMap::new();
        let lib_placeholders = std::iter::repeat_n("?", visible_libs.len())
            .collect::<Vec<_>>()
            .join(",");
        for chunk in candidates.chunks(200) {
            let tuple_placeholders = std::iter::repeat_n("(?,?,?)", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT a.source, a.kind, a.external_id,
                        MIN(f.media_id) AS media_item_id,
                        COUNT(DISTINCT f.library_id) AS library_count,
                        COUNT(f.file_id) AS file_count
                   FROM media_aliases a
                   JOIN library_files f
                     ON f.media_id = a.media_id
                  WHERE f.publication_state = 'published'
                    AND f.presence_state != 'missing'
                    AND f.library_id IN ({lib_placeholders})
                    AND (a.source, a.kind, a.external_id) IN ({tuple_placeholders})
                  GROUP BY a.source, a.kind, a.external_id"
            );
            let mut bind_values = Vec::with_capacity(visible_libs.len() + chunk.len() * 3);
            for lib_id in &visible_libs {
                bind_values.push(lib_id.as_str());
            }
            for (source, kind, ext_id) in chunk {
                bind_values.push(source.as_str());
                bind_values.push(kind.as_str());
                bind_values.push(ext_id.as_str());
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(bind_values), |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ),
                    OwnedSummary {
                        media_item_id: row.get(3)?,
                        library_count: row.get(4)?,
                        file_count: row.get(5)?,
                    },
                ))
            })?;
            for row in rows {
                let (key, summary) = row?;
                owned.insert(key, summary);
            }
        }
        Ok(owned)
    })
}

fn apply_ownership_to_items(items: &mut [Value], owned: &HashMap<AliasKey, OwnedSummary>) {
    for item in items {
        let matched = (|| {
            let kind = item.get("kind")?.as_str()?;
            if let Some(tmdb_id) = item
                .get("tmdb_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                && let Some(summary) =
                    owned.get(&("tmdb".to_string(), kind.to_string(), tmdb_id.to_string()))
            {
                return Some(summary);
            }
            if let Some(douban_id) = item
                .get("douban_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                && let Some(summary) = owned.get(&(
                    "douban".to_string(),
                    kind.to_string(),
                    douban_id.to_string(),
                ))
            {
                return Some(summary);
            }
            None
        })()
        .cloned();

        if let Some(map) = item.as_object_mut() {
            if let Some(summary) = matched {
                map.insert("in_library".into(), Value::Bool(true));
                map.insert(
                    "library_status".into(),
                    json!({
                        "owned": true,
                        "media_item_id": summary.media_item_id,
                        "library_count": summary.library_count,
                        "file_count": summary.file_count,
                    }),
                );
            } else {
                map.insert("in_library".into(), Value::Bool(false));
            }
        }
    }
}

fn apply_ownership_to_sections(sections: &mut [Value], owned: &HashMap<AliasKey, OwnedSummary>) {
    for section in sections {
        if let Some(items) = section.get_mut("items").and_then(Value::as_array_mut) {
            apply_ownership_to_items(items, owned);
        }
    }
}

async fn enrich_items_ownership(
    state: &ApiState,
    viewer: Option<domain::UserId>,
    items: &mut Vec<Value>,
) -> Result<(), Response> {
    let mut candidates = Vec::new();
    collect_item_alias_keys(items, &mut candidates);
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.is_empty() {
        apply_ownership_to_items(items, &HashMap::new());
        return Ok(());
    }
    let taken = std::mem::take(items);
    let enriched =
        super::store_wait::store_read(state, &[store::Db::Library, store::Db::App], move |store| {
            let mut batch = taken;
            let owned = owned_aliases_in_visible_libraries(store, viewer, &candidates)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "发现列表批量入库状态查询失败，降级为无标记");
                    HashMap::new()
                });
            apply_ownership_to_items(&mut batch, &owned);
            batch
        })
        .await
        .map_err(super::store_wait::StoreCallError::response)?;
    *items = enriched;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FilterQuery, build_filter_query_string};

    #[test]
    fn filter_query_string_encodes_injected_delimiters_in_genres_and_country() {
        let q = FilterQuery {
            genres: Some("28,12&include_adult=true#frag".into()),
            country: Some("CN?api_key=leak&page=9".into()),
            ..Default::default()
        };
        let qs = build_filter_query_string("movie", &q);
        assert!(!qs.contains("&include_adult="), "{qs}");
        assert!(!qs.contains("&page="), "{qs}");
        assert!(!qs.contains('#'), "{qs}");
        assert!(!qs.contains('?'), "{qs}");
        assert!(qs.starts_with("with_genres=28,12%26include_adult%3Dtrue%23frag&"));
        assert!(qs.contains("&with_origin_country=CN%3Fapi_key%3Dleak%26page%3D9&"));
    }
}
