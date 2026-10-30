//! Fixed discover section orchestration: the section metadata tables and the
//! four source-specific wall builders (TMDB movie/TV, Douban movie/TV), in the
//! source and section order the wall renders.

use crate::catalog::Catalog;

#[derive(Clone, Copy)]
pub(super) struct Section {
    pub kind: &'static str,
    pub id: &'static str,
    pub title: &'static str,
    pub presentation: &'static str,
    pub fetch: fn(&dyn Catalog) -> Result<Vec<media::CatalogHit>, String>,
}

/// TMDB discover query for a section id, when the section is paginable.
/// `None` = hero/featured only (no full listing). Owned `String`: sections are
/// fixed at compile time, but the composed query must not be leaked.
pub(super) fn section_query(kind: &str, id: &str) -> Option<String> {
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
        Some(sort.to_string())
    } else {
        Some(format!("{base}&{sort}"))
    }
}

/// Whether a section id supports a full listing page.
/// Whether a section id supports a full listing page.
///
/// TMDB sources: every discover-backed section is paginable (hero 除外).
/// Douban: only sections Douban's search_subjects can paginate get a
/// listing link — 豆瓣高分 / Top250 类。其他分区（实时热门/类型榜…）豆瓣
/// 无法翻页，不给入口，避免点进去却落到 TMDB 数据。
pub(super) fn supports_full_listing(source: &str, kind: &str, id: &str) -> bool {
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
pub(super) fn collection_title(kind: &str, id: &str) -> String {
    section_title(kind, id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

const MOVIE_SECTIONS: &[Section] = &[
    Section {
        kind: "movie",
        id: "featured-weekly",
        title: "本周精选",
        presentation: "hero",
        fetch: |catalog| catalog.trending_movie(),
    },
    Section {
        kind: "movie",
        id: "trending-day",
        title: "今日热榜",
        presentation: "ranked-row",
        fetch: |catalog| catalog.trending_movie(),
    },
    Section {
        kind: "movie",
        id: "now-playing",
        title: "正在热映",
        presentation: "poster-row",
        fetch: |catalog| catalog.now_playing_movie(),
    },
    Section {
        kind: "movie",
        id: "upcoming",
        title: "即将上映",
        presentation: "poster-row",
        fetch: |catalog| catalog.upcoming_movie(),
    },
    Section {
        kind: "movie",
        id: "popular",
        title: "热门电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.popular_movie(),
    },
    Section {
        kind: "movie",
        id: "recent-acclaimed",
        title: "近两年高口碑",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_movie(
                "primary_release_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=500",
            )
        },
    },
    Section {
        kind: "movie",
        id: "this-year",
        title: "今年新片",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_movie("primary_release_year=2026&sort_by=popularity.desc")
        },
    },
    Section {
        kind: "movie",
        id: "short-runtime",
        title: "90 分钟以内",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_runtime.lte=90&sort_by=vote_average.desc"),
    },
    Section {
        kind: "movie",
        id: "top-rated",
        title: "高分经典",
        presentation: "poster-row",
        fetch: |catalog| catalog.top_rated_movie(),
    },
    Section {
        kind: "movie",
        id: "chinese",
        title: "华语佳片",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_movie("with_original_language=zh&sort_by=popularity.desc")
        },
    },
    Section {
        kind: "movie",
        id: "japanese",
        title: "日本电影",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_movie("with_original_language=ja&sort_by=popularity.desc")
        },
    },
    Section {
        kind: "movie",
        id: "korean",
        title: "韩国电影",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_movie("with_original_language=ko&sort_by=popularity.desc")
        },
    },
    Section {
        kind: "movie",
        id: "scifi",
        title: "科幻巨制",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=878&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "action",
        title: "动作与冒险",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=28&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "thriller",
        title: "悬疑惊悚",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=53&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "comedy",
        title: "喜剧佳作",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=35&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "romance",
        title: "爱情电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=10749&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "horror",
        title: "恐怖片",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=27&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "animation",
        title: "动画电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=16&sort_by=popularity.desc"),
    },
    Section {
        kind: "movie",
        id: "documentary",
        title: "纪录片",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_movie("with_genres=99&sort_by=popularity.desc"),
    },
];

pub(super) fn movie_sections() -> Vec<Section> {
    MOVIE_SECTIONS.to_vec()
}

const TV_SECTIONS: &[Section] = &[
    Section {
        kind: "tv",
        id: "featured-weekly",
        title: "本周精选",
        presentation: "hero",
        fetch: |catalog| catalog.trending_tv(),
    },
    Section {
        kind: "tv",
        id: "trending-day",
        title: "今日热榜",
        presentation: "ranked-row",
        fetch: |catalog| catalog.trending_tv(),
    },
    Section {
        kind: "tv",
        id: "airing-today",
        title: "今日更新",
        presentation: "poster-row",
        fetch: |catalog| catalog.airing_today_tv(),
    },
    Section {
        kind: "tv",
        id: "on-the-air",
        title: "本周更新",
        presentation: "poster-row",
        fetch: |catalog| catalog.on_the_air_tv(),
    },
    Section {
        kind: "tv",
        id: "popular",
        title: "热门剧集",
        presentation: "poster-row",
        fetch: |catalog| catalog.popular_tv(),
    },
    Section {
        kind: "tv",
        id: "recent-acclaimed",
        title: "近两年高口碑",
        presentation: "poster-row",
        fetch: |catalog| {
            catalog.filtered_tv(
                "first_air_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=300",
            )
        },
    },
    Section {
        kind: "tv",
        id: "completed-miniseries",
        title: "已完结迷你剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_type=2&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "top-rated",
        title: "高分神剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.top_rated_tv(),
    },
    Section {
        kind: "tv",
        id: "chinese",
        title: "华语剧集",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_original_language=zh&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "us",
        title: "热门美剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_origin_country=US&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "japanese",
        title: "热门日剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_original_language=ja&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "korean",
        title: "热门韩剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_original_language=ko&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "netflix",
        title: "Netflix 出品",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_networks=213&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "hbo",
        title: "HBO 出品",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_networks=49&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "scifi-fantasy",
        title: "科幻与奇幻",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=10765&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "crime-mystery",
        title: "悬疑罪案",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=80&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "comedy",
        title: "喜剧剧集",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=35&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "animation",
        title: "动画剧集",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=16&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "reality",
        title: "真人秀",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=10764&sort_by=popularity.desc"),
    },
    Section {
        kind: "tv",
        id: "documentary",
        title: "纪录片",
        presentation: "poster-row",
        fetch: |catalog| catalog.filtered_tv("with_genres=99&sort_by=popularity.desc"),
    },
];

pub(super) fn tv_sections() -> Vec<Section> {
    TV_SECTIONS.to_vec()
}

/// 豆瓣源独立分区表（电影）：直接用豆瓣 tag，标题用豆瓣风格，全真实数据。
const DOUBAN_MOVIE_SECTIONS: &[Section] = &[
    Section {
        kind: "movie",
        id: "featured-weekly",
        title: "本周精选",
        presentation: "hero",
        fetch: |catalog| catalog.tagged_movie("热门"),
    },
    Section {
        kind: "movie",
        id: "trending-day",
        title: "实时热榜",
        presentation: "ranked-row",
        fetch: |catalog| catalog.trending_movie(),
    },
    Section {
        kind: "movie",
        id: "now-playing",
        title: "正在热映",
        presentation: "poster-row",
        fetch: |catalog| catalog.now_playing_movie(),
    },
    Section {
        kind: "movie",
        id: "popular",
        title: "热门电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("热门"),
    },
    Section {
        kind: "movie",
        id: "top-rated",
        title: "豆瓣高分",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("豆瓣高分"),
    },
    Section {
        kind: "movie",
        id: "classic",
        title: "经典电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("经典"),
    },
    Section {
        kind: "movie",
        id: "chinese",
        title: "华语佳片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("华语"),
    },
    Section {
        kind: "movie",
        id: "japanese",
        title: "日本电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("日本"),
    },
    Section {
        kind: "movie",
        id: "korean",
        title: "韩国电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("韩国"),
    },
    Section {
        kind: "movie",
        id: "us",
        title: "欧美佳片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("欧美"),
    },
    Section {
        kind: "movie",
        id: "cold",
        title: "冷门佳片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("冷门佳片"),
    },
    Section {
        kind: "movie",
        id: "drama",
        title: "剧情片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("剧情"),
    },
    Section {
        kind: "movie",
        id: "crime",
        title: "犯罪片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("犯罪"),
    },
    Section {
        kind: "movie",
        id: "fantasy",
        title: "奇幻片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("奇幻"),
    },
    Section {
        kind: "movie",
        id: "scifi",
        title: "科幻巨制",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("科幻"),
    },
    Section {
        kind: "movie",
        id: "action",
        title: "动作与冒险",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("动作"),
    },
    Section {
        kind: "movie",
        id: "thriller",
        title: "悬疑惊悚",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("悬疑"),
    },
    Section {
        kind: "movie",
        id: "comedy",
        title: "喜剧佳作",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("喜剧"),
    },
    Section {
        kind: "movie",
        id: "romance",
        title: "爱情电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("爱情"),
    },
    Section {
        kind: "movie",
        id: "horror",
        title: "恐怖片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("恐怖"),
    },
    Section {
        kind: "movie",
        id: "animation",
        title: "动画电影",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("动画"),
    },
    Section {
        kind: "movie",
        id: "documentary",
        title: "纪录片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("纪录片"),
    },
    Section {
        kind: "movie",
        id: "war",
        title: "战争片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("战争"),
    },
    Section {
        kind: "movie",
        id: "music",
        title: "音乐片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("音乐"),
    },
    Section {
        kind: "movie",
        id: "imax",
        title: "IMAX 精选",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("IMAX"),
    },
    Section {
        kind: "movie",
        id: "4k",
        title: "4K 片单",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_movie("4K"),
    },
];

pub(super) fn douban_movie_sections() -> Vec<Section> {
    DOUBAN_MOVIE_SECTIONS.to_vec()
}

/// 豆瓣源独立分区表（剧集）：直接用豆瓣 tag 与榜单，全真实数据。
const DOUBAN_TV_SECTIONS: &[Section] = &[
    Section {
        kind: "tv",
        id: "featured-weekly",
        title: "本周精选",
        presentation: "hero",
        fetch: |catalog| catalog.tagged_tv("热门"),
    },
    Section {
        kind: "tv",
        id: "trending-day",
        title: "实时热榜",
        presentation: "ranked-row",
        fetch: |catalog| catalog.trending_tv(),
    },
    Section {
        kind: "tv",
        id: "popular",
        title: "热门剧集",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("热门"),
    },
    Section {
        kind: "tv",
        id: "chinese",
        title: "国产剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("国产剧"),
    },
    Section {
        kind: "tv",
        id: "us",
        title: "美剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("美剧"),
    },
    Section {
        kind: "tv",
        id: "japanese",
        title: "日剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("日剧"),
    },
    Section {
        kind: "tv",
        id: "korean",
        title: "韩剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("韩剧"),
    },
    Section {
        kind: "tv",
        id: "uk",
        title: "英剧",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("英剧"),
    },
    Section {
        kind: "tv",
        id: "animation",
        title: "动漫",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("动漫"),
    },
    Section {
        kind: "tv",
        id: "documentary",
        title: "纪录片",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("纪录片"),
    },
    Section {
        kind: "tv",
        id: "variety",
        title: "综艺",
        presentation: "poster-row",
        fetch: |catalog| catalog.tagged_tv("综艺"),
    },
];

pub(super) fn douban_tv_sections() -> Vec<Section> {
    DOUBAN_TV_SECTIONS.to_vec()
}
