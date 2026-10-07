//! Discover: catalog rows for movie / TV walls, optionally one source.
//! Section table mirrors the upstream discover page (20 movie + 20 TV rows).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use media::CatalogHit;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::http::{err, ok};
use crate::management::ApiState;

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";
const TMDB_BACKDROP: &str = "https://image.tmdb.org/t/p/w1280";

#[derive(Deserialize)]
pub(crate) struct DiscoverQuery {
    /// "tmdb" | "douban" | absent = all sources merged.
    source: Option<String>,
}

pub(crate) async fn discover_kind(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Query(query): Query<DiscoverQuery>,
) -> Response {
    // 分区墙一次串行拉 20 个分区，每个都是同步 TMDB/豆瓣 HTTP 请求
    //（ureq 阻塞 1-8s）。直接在 async handler 里跑会占满 tokio 工作线程，
    // 其他请求（包括无锁的 /health）全部排队超时——表现为「正在连接服务…」。
    // 挪到 spawn_blocking 池，worker 线程不被占用。
    // 错误码区分：未知 kind/source 是客户端错（400），上游失败是 502。
    let state_clone = state.clone();
    let kind_clone = kind.clone();
    let source = query.source.clone();
    let sections = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, (u16, String)> {
        let mut catalog: &dyn Catalog = state_clone.catalog.as_ref();
        if let Some(source) = source.as_deref().filter(|source| !source.is_empty()) {
            match catalog.source(source) {
                Some(found) => catalog = found,
                None if catalog.source_name() == source => {}
                None => return Err((400, format!("未知数据源: {source}"))),
            }
        }
        match kind_clone.as_str() {
            // 豆瓣源用独立分区表（真实豆瓣 tag），不走 TMDB 分区映射。
            "movie" if catalog.source_name() == "douban" => Ok(douban_movie_sections(catalog)),
            "tv" if catalog.source_name() == "douban" => Ok(douban_tv_sections(catalog)),
            "movie" => Ok(movie_sections("movie", catalog)),
            "tv" => Ok(tv_sections("tv", catalog)),
            _ => Err((400, "kind 必须是 movie 或 tv".into())),
        }
    })
    .await
    .unwrap_or_else(|_| Err((502, "分区加载被中断".into())));
    let sections = match sections {
        Ok(sections) => sections,
        Err((status, message)) => {
            return err(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                "discover.upstream",
                &message,
            );
        }
    };
    let tmdb_configured = state
        .store
        .lock()
        .get_setting(crate::settings_keys::TMDB_API_KEY)
        .ok()
        .flatten()
        .map(|value| !value.is_empty())
        .unwrap_or(false);
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

/// TMDB discover query for a section id, when the section is paginable.
/// `None` = hero/featured only (no full listing).
fn section_query(kind: &str, id: &str) -> Option<&'static str> {
    let (base, sort) = match id {
        "trending-day" => ("", "sort_by=popularity.desc"),
        "now-playing" if kind == "movie" => ("", "sort_by=popularity.desc&with_release_type=3"),
        "upcoming" if kind == "movie" => ("", "sort_by=popularity.desc&with_release_type=2"),
        "airing-today" if kind == "tv" => ("", "sort_by=popularity.desc"),
        "on-the-air" if kind == "tv" => ("", "sort_by=popularity.desc"),
        "popular" => ("", "sort_by=popularity.desc"),
        "top-rated" => ("", "sort_by=vote_average.desc"),
        "recent-acclaimed" => (
            if kind == "movie" {
                "primary_release_date.gte=2024-09-20"
            } else {
                "first_air_date.gte=2024-09-20"
            },
            "sort_by=vote_average.desc&vote_count.gte=100",
        ),
        "this-year" if kind == "movie" => ("primary_release_year=2026", "sort_by=popularity.desc"),
        "short-runtime" if kind == "movie" => ("with_runtime.lte=90", "sort_by=vote_average.desc"),
        "chinese" => ("with_original_language=zh", "sort_by=popularity.desc"),
        "japanese" if kind == "movie" => ("with_original_language=ja", "sort_by=popularity.desc"),
        "korean" if kind == "movie" => ("with_original_language=ko", "sort_by=popularity.desc"),
        "us" if kind == "tv" => ("with_origin_country=US", "sort_by=popularity.desc"),
        "japanese" if kind == "tv" => ("with_original_language=ja", "sort_by=popularity.desc"),
        "korean" if kind == "tv" => ("with_original_language=ko", "sort_by=popularity.desc"),
        "netflix" if kind == "tv" => ("with_networks=213", "sort_by=popularity.desc"),
        "hbo" if kind == "tv" => ("with_networks=49", "sort_by=popularity.desc"),
        "scifi" if kind == "movie" => ("with_genres=878", "sort_by=popularity.desc"),
        "scifi-fantasy" if kind == "tv" => ("with_genres=10765", "sort_by=popularity.desc"),
        "scifi" | "scifi-fantasy" => ("with_genres=878", "sort_by=popularity.desc"),
        "action" => ("with_genres=28", "sort_by=popularity.desc"),
        "thriller" => ("with_genres=53", "sort_by=popularity.desc"),
        "crime-mystery" => ("with_genres=80", "sort_by=popularity.desc"),
        "comedy" => ("with_genres=35", "sort_by=popularity.desc"),
        "romance" => ("with_genres=10749", "sort_by=popularity.desc"),
        "horror" => ("with_genres=27", "sort_by=popularity.desc"),
        "animation" => ("with_genres=16", "sort_by=popularity.desc"),
        "documentary" => ("with_genres=99", "sort_by=popularity.desc"),
        "reality" => ("with_genres=10764", "sort_by=popularity.desc"),
        _ => return None,
    };
    if base.is_empty() {
        Some(leak_query(sort))
    } else {
        Some(leak_query(&format!("{base}&{sort}")))
    }
}

/// Build a static query string (sections are fixed at compile time).
fn leak_query(query: &str) -> &'static str {
    Box::leak(query.to_string().into_boxed_str())
}

/// Whether a section id supports a full listing page.
/// Whether a section id supports a full listing page.
///
/// TMDB sources: every discover-backed section is paginable (hero 除外).
/// Douban: only sections Douban's search_subjects can paginate get a
/// listing link — 豆瓣高分 / Top250 类。其他分区（实时热门/类型榜…）豆瓣
/// 无法翻页，不给入口，避免点进去却落到 TMDB 数据。
fn supports_full_listing(source: &str, kind: &str, id: &str) -> bool {
    if source == "douban" {
        return id == "top-rated"; // 豆瓣高分经典
    }
    section_query(kind, id).is_some()
}

/// Chinese title for a section id (full listing page header).
fn section_title(kind: &str, id: &str) -> Option<&'static str> {
    let title = match id {
        "trending-day" => "今日热榜",
        "now-playing" if kind == "movie" => "正在热映",
        "upcoming" if kind == "movie" => "即将上映",
        "airing-today" if kind == "tv" => "今日首播",
        "on-the-air" if kind == "tv" => "正在播出",
        "popular" if kind == "movie" => "热门电影",
        "popular" => "热门剧集",
        "top-rated" => "高分经典",
        "recent-acclaimed" => "近两年高口碑",
        "this-year" if kind == "movie" => "今年新片",
        "short-runtime" if kind == "movie" => "90 分钟以内",
        "chinese" => "华语佳片",
        "japanese" if kind == "movie" => "日本电影",
        "korean" if kind == "movie" => "韩国电影",
        "us" if kind == "tv" => "美国剧集",
        "japanese" if kind == "tv" => "日本剧集",
        "korean" if kind == "tv" => "韩国剧集",
        "netflix" => "Netflix 剧集",
        "hbo" => "HBO 剧集",
        "scifi" if kind == "movie" => "科幻巨制",
        "scifi-fantasy" if kind == "tv" => "科幻与奇幻",
        "scifi" | "scifi-fantasy" => "科幻巨制",
        "action" => "动作与冒险",
        "thriller" => "悬疑惊悚",
        "crime-mystery" => "犯罪悬疑",
        "comedy" => "喜剧佳作",
        "romance" => "爱情电影",
        "horror" => "恐怖片",
        "animation" if kind == "movie" => "动画电影",
        "animation" => "动画剧集",
        "documentary" => "纪录片",
        "reality" => "真人秀",
        _ => return None,
    };
    Some(title)
}

/// Per-kind full-listing titles (used by the listing page header).
fn collection_title(kind: &str, id: &str) -> String {
    section_title(kind, id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

fn movie_sections(_kind: &str, catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            catalog.source_name(),
            "movie",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.trending_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "trending-day",
            "今日热榜",
            "ranked-row",
            catalog.trending_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "now-playing",
            "正在热映",
            "poster-row",
            catalog.now_playing_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "upcoming",
            "即将上映",
            "poster-row",
            catalog.upcoming_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "popular",
            "热门电影",
            "poster-row",
            catalog.popular_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "recent-acclaimed",
            "近两年高口碑",
            "poster-row",
            catalog.filtered_movie(
                "primary_release_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=500",
            ),
        ),
        row(
            catalog.source_name(),
            "movie",
            "this-year",
            "今年新片",
            "poster-row",
            catalog.filtered_movie("primary_release_year=2026&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "short-runtime",
            "90 分钟以内",
            "poster-row",
            catalog.filtered_movie("with_runtime.lte=90&sort_by=vote_average.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "top-rated",
            "高分经典",
            "poster-row",
            catalog.top_rated_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "chinese",
            "华语佳片",
            "poster-row",
            catalog.filtered_movie("with_original_language=zh&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "japanese",
            "日本电影",
            "poster-row",
            catalog.filtered_movie("with_original_language=ja&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "korean",
            "韩国电影",
            "poster-row",
            catalog.filtered_movie("with_original_language=ko&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "scifi",
            "科幻巨制",
            "poster-row",
            catalog.filtered_movie("with_genres=878&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "action",
            "动作与冒险",
            "poster-row",
            catalog.filtered_movie("with_genres=28&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "thriller",
            "悬疑惊悚",
            "poster-row",
            catalog.filtered_movie("with_genres=53&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "comedy",
            "喜剧佳作",
            "poster-row",
            catalog.filtered_movie("with_genres=35&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "romance",
            "爱情电影",
            "poster-row",
            catalog.filtered_movie("with_genres=10749&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "horror",
            "恐怖片",
            "poster-row",
            catalog.filtered_movie("with_genres=27&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "animation",
            "动画电影",
            "poster-row",
            catalog.filtered_movie("with_genres=16&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.filtered_movie("with_genres=99&sort_by=popularity.desc"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

fn tv_sections(_kind: &str, catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            catalog.source_name(),
            "tv",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.trending_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "trending-day",
            "今日热榜",
            "ranked-row",
            catalog.trending_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "airing-today",
            "今日更新",
            "poster-row",
            catalog.airing_today_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "on-the-air",
            "本周更新",
            "poster-row",
            catalog.on_the_air_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "popular",
            "热门剧集",
            "poster-row",
            catalog.popular_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "recent-acclaimed",
            "近两年高口碑",
            "poster-row",
            catalog.filtered_tv(
                "first_air_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=300",
            ),
        ),
        row(
            catalog.source_name(),
            "tv",
            "completed-miniseries",
            "已完结迷你剧",
            "poster-row",
            catalog.filtered_tv("with_type=2&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "top-rated",
            "高分神剧",
            "poster-row",
            catalog.top_rated_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "chinese",
            "华语剧集",
            "poster-row",
            catalog.filtered_tv("with_original_language=zh&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "us",
            "热门美剧",
            "poster-row",
            catalog.filtered_tv("with_origin_country=US&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "japanese",
            "热门日剧",
            "poster-row",
            catalog.filtered_tv("with_original_language=ja&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "korean",
            "热门韩剧",
            "poster-row",
            catalog.filtered_tv("with_original_language=ko&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "netflix",
            "Netflix 出品",
            "poster-row",
            catalog.filtered_tv("with_networks=213&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "hbo",
            "HBO 出品",
            "poster-row",
            catalog.filtered_tv("with_networks=49&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "scifi-fantasy",
            "科幻与奇幻",
            "poster-row",
            catalog.filtered_tv("with_genres=10765&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "crime-mystery",
            "悬疑罪案",
            "poster-row",
            catalog.filtered_tv("with_genres=80&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "comedy",
            "喜剧剧集",
            "poster-row",
            catalog.filtered_tv("with_genres=35&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "animation",
            "动画剧集",
            "poster-row",
            catalog.filtered_tv("with_genres=16&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "reality",
            "真人秀",
            "poster-row",
            catalog.filtered_tv("with_genres=10764&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.filtered_tv("with_genres=99&sort_by=popularity.desc"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

/// 豆瓣源独立分区表（电影）：直接用豆瓣 tag，标题用豆瓣风格，全真实数据。
fn douban_movie_sections(catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            "douban",
            "movie",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.tagged_movie("热门"),
        ),
        row(
            "douban",
            "movie",
            "trending-day",
            "实时热榜",
            "ranked-row",
            catalog.trending_movie(),
        ),
        row(
            "douban",
            "movie",
            "now-playing",
            "正在热映",
            "poster-row",
            catalog.now_playing_movie(),
        ),
        row(
            "douban",
            "movie",
            "popular",
            "热门电影",
            "poster-row",
            catalog.tagged_movie("热门"),
        ),
        row(
            "douban",
            "movie",
            "top-rated",
            "豆瓣高分",
            "poster-row",
            catalog.tagged_movie("豆瓣高分"),
        ),
        row(
            "douban",
            "movie",
            "classic",
            "经典电影",
            "poster-row",
            catalog.tagged_movie("经典"),
        ),
        row(
            "douban",
            "movie",
            "chinese",
            "华语佳片",
            "poster-row",
            catalog.tagged_movie("华语"),
        ),
        row(
            "douban",
            "movie",
            "japanese",
            "日本电影",
            "poster-row",
            catalog.tagged_movie("日本"),
        ),
        row(
            "douban",
            "movie",
            "korean",
            "韩国电影",
            "poster-row",
            catalog.tagged_movie("韩国"),
        ),
        row(
            "douban",
            "movie",
            "us",
            "欧美佳片",
            "poster-row",
            catalog.tagged_movie("欧美"),
        ),
        row(
            "douban",
            "movie",
            "cold",
            "冷门佳片",
            "poster-row",
            catalog.tagged_movie("冷门佳片"),
        ),
        row(
            "douban",
            "movie",
            "drama",
            "剧情片",
            "poster-row",
            catalog.tagged_movie("剧情"),
        ),
        row(
            "douban",
            "movie",
            "crime",
            "犯罪片",
            "poster-row",
            catalog.tagged_movie("犯罪"),
        ),
        row(
            "douban",
            "movie",
            "fantasy",
            "奇幻片",
            "poster-row",
            catalog.tagged_movie("奇幻"),
        ),
        row(
            "douban",
            "movie",
            "scifi",
            "科幻巨制",
            "poster-row",
            catalog.tagged_movie("科幻"),
        ),
        row(
            "douban",
            "movie",
            "action",
            "动作与冒险",
            "poster-row",
            catalog.tagged_movie("动作"),
        ),
        row(
            "douban",
            "movie",
            "thriller",
            "悬疑惊悚",
            "poster-row",
            catalog.tagged_movie("悬疑"),
        ),
        row(
            "douban",
            "movie",
            "comedy",
            "喜剧佳作",
            "poster-row",
            catalog.tagged_movie("喜剧"),
        ),
        row(
            "douban",
            "movie",
            "romance",
            "爱情电影",
            "poster-row",
            catalog.tagged_movie("爱情"),
        ),
        row(
            "douban",
            "movie",
            "horror",
            "恐怖片",
            "poster-row",
            catalog.tagged_movie("恐怖"),
        ),
        row(
            "douban",
            "movie",
            "animation",
            "动画电影",
            "poster-row",
            catalog.tagged_movie("动画"),
        ),
        row(
            "douban",
            "movie",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.tagged_movie("纪录片"),
        ),
        row(
            "douban",
            "movie",
            "war",
            "战争片",
            "poster-row",
            catalog.tagged_movie("战争"),
        ),
        row(
            "douban",
            "movie",
            "music",
            "音乐片",
            "poster-row",
            catalog.tagged_movie("音乐"),
        ),
        row(
            "douban",
            "movie",
            "imax",
            "IMAX 精选",
            "poster-row",
            catalog.tagged_movie("IMAX"),
        ),
        row(
            "douban",
            "movie",
            "4k",
            "4K 片单",
            "poster-row",
            catalog.tagged_movie("4K"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

/// 豆瓣源独立分区表（剧集）：直接用豆瓣 tag 与榜单，全真实数据。
fn douban_tv_sections(catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            "douban",
            "tv",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.tagged_tv("热门"),
        ),
        row(
            "douban",
            "tv",
            "trending-day",
            "实时热榜",
            "ranked-row",
            catalog.trending_tv(),
        ),
        row(
            "douban",
            "tv",
            "popular",
            "热门剧集",
            "poster-row",
            catalog.tagged_tv("热门"),
        ),
        row(
            "douban",
            "tv",
            "chinese",
            "国产剧",
            "poster-row",
            catalog.tagged_tv("国产剧"),
        ),
        row(
            "douban",
            "tv",
            "us",
            "美剧",
            "poster-row",
            catalog.tagged_tv("美剧"),
        ),
        row(
            "douban",
            "tv",
            "japanese",
            "日剧",
            "poster-row",
            catalog.tagged_tv("日剧"),
        ),
        row(
            "douban",
            "tv",
            "korean",
            "韩剧",
            "poster-row",
            catalog.tagged_tv("韩剧"),
        ),
        row(
            "douban",
            "tv",
            "uk",
            "英剧",
            "poster-row",
            catalog.tagged_tv("英剧"),
        ),
        row(
            "douban",
            "tv",
            "animation",
            "动漫",
            "poster-row",
            catalog.tagged_tv("动漫"),
        ),
        row(
            "douban",
            "tv",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.tagged_tv("纪录片"),
        ),
        row(
            "douban",
            "tv",
            "variety",
            "综艺",
            "poster-row",
            catalog.tagged_tv("综艺"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

fn item_json(hit: CatalogHit) -> Value {
    let id = hit
        .media
        .tmdb_id
        .clone()
        .or(hit.media.douban_id.clone())
        .unwrap_or_else(|| hit.media.id.to_string());
    json!({
        "id": id,
        "title": hit.media.title,
        "year": hit.media.year,
        "kind": hit.media.kind.as_str(),
        "tmdb_id": hit.media.tmdb_id,
        "poster_url": poster_url(hit.poster_path.as_deref()),
        "backdrop_url": backdrop_url(hit.backdrop_path.as_deref()),
        "rating": hit.rating,
        "overview": hit.overview,
    })
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
    genres: Option<String>,
    country: Option<String>,
    year: Option<u32>,
    rating: Option<f32>,
    runtime: Option<u32>,
    sort: Option<String>,
    page: Option<i64>,
}

/// Filtered wall: `GET /discover/{kind}/filtered?genres=..&country=..&year=..
/// &rating=..&runtime=..&sort=..`. Only TMDB sources implement real filters;
/// other unsupported sources are safely ignored in fanout merge, returning empty list (200) instead of 502.
pub(crate) async fn discover_filtered(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Query(query): Query<FilterQuery>,
) -> Response {
    let mut params = Vec::new();
    if let Some(genres) = query.genres.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("with_genres={genres}"));
    }
    // TV 且用户没指定类型时，默认排除综艺/真人秀（TMDB 的 CN origin 大量是
    // 这些，用户筛「中国剧」时几乎总是想要剧情剧而不是《奔跑吧》）。
    if kind.as_str() == "tv" && query.genres.as_deref().unwrap_or("").is_empty() {
        params.push("without_genres=10764,10767".to_string());
    }
    if let Some(country) = query.country.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("with_origin_country={country}"));
    }
    let is_tv = kind.as_str() == "tv";
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
        // TV 没有 primary_release_year，只有 first_air_date_year。
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
    let query_string = params.join("&");
    let page = query.page.unwrap_or(1).max(1);
    let state_clone = state.clone();
    let kind_clone = kind.clone();
    let qs_clone = query_string.clone();
    let paged_res = tokio::task::spawn_blocking(
        move || -> Result<(Vec<CatalogHit>, i64, i64), (u16, String)> {
            let catalog: &dyn Catalog = state_clone.catalog.as_ref();
            match kind_clone.as_str() {
                "movie" => {
                    match catalog.collection_paged(domain::MediaKind::Movie, &qs_clone, page) {
                        Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
                        _ => {
                            let hits = catalog.filtered_movie(&qs_clone).map_err(|e| (502, e))?;
                            let len = hits.len() as i64;
                            Ok((hits, if len > 0 { 1 } else { 0 }, len))
                        }
                    }
                }
                "tv" => match catalog.collection_paged(domain::MediaKind::Tv, &qs_clone, page) {
                    Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
                    _ => {
                        let hits = catalog.filtered_tv(&qs_clone).map_err(|e| (502, e))?;
                        let len = hits.len() as i64;
                        Ok((hits, if len > 0 { 1 } else { 0 }, len))
                    }
                },
                _ => Err((400, "kind 必须是 movie 或 tv".into())),
            }
        },
    )
    .await
    .unwrap_or_else(|_| Err((502, "筛选请求被中断".into())));

    let (hits, total_pages, total_results) = match paged_res {
        Ok(res) => res,
        Err((status, message)) => {
            return err(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                "discover.upstream",
                &message,
            );
        }
    };
    ok(json!({
        "kind": kind,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
        "has_more": page < total_pages,
        "items": hits.into_iter().map(item_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `GET /discover/{kind}/collection/{id}?source=&page=1&page_size=20` — a full
/// listing for one wall section (分页). 404 when the section has no listing.
/// source=douban 走豆瓣翻页（仅可翻页分区有入口），否则 TMDB discover。
pub(crate) async fn discover_collection(
    State(state): State<ApiState>,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<CollectionQuery>,
) -> Response {
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
    if let Some(source) = query.source.as_deref().filter(|source| !source.is_empty()) {
        match catalog.source(source) {
            Some(found) => catalog = found,
            None if catalog.source_name() == source => {}
            None => return err(StatusCode::BAD_REQUEST, "discover.source", "未知数据源"),
        }
    }
    let is_douban = catalog.source_name() == "douban";
    let normalized_id = match id.as_str() {
        "movie_high_score" | "movie_top250" => "top-rated",
        other => other,
    };
    // 豆瓣源只有高分分区有完整榜单；其他分区（走 TMDB query 或豆瓣无翻页 tag）拒绝。
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
    let media_kind_clone = media_kind;
    let section_query_owned = section_query.to_string();
    let source_clone = query.source.clone();
    let paged = tokio::task::spawn_blocking(move || {
        let mut catalog: &dyn Catalog = state_clone.catalog.as_ref();
        if let Some(source) = source_clone.as_deref().filter(|source| !source.is_empty()) {
            if let Some(found) = catalog.source(source) {
                catalog = found;
            }
        }
        catalog.collection_paged(media_kind_clone, &section_query_owned, page)
    })
    .await
    .unwrap_or_else(|_| Err("榜单请求被中断".into()));
    let paged = match paged {
        Ok(Some(result)) => result,
        Ok(None) => {
            return err(
                StatusCode::NOT_FOUND,
                "discover.collection",
                "该分区不支持完整榜单",
            );
        }
        Err(error) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
    };
    let (hits, _, total_pages, total_results) = paged;
    let items: Vec<Value> = hits
        .into_iter()
        .take(page_size as usize)
        .map(item_json)
        .collect();
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
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let rows = match catalog.genres(media_kind) {
        Ok(rows) => rows,
        Err(error) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ok(json!({ "kind": kind, "genres": items })).into_response()
}
