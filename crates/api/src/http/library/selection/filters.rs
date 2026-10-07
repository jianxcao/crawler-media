use super::{LibrarySelection, best_resolution, resolution_rank};
use std::{cmp::Ordering, collections::HashMap};

pub(crate) struct LibraryFilter {
    lists: HashMap<String, Vec<String>>,
    watch: Option<String>,
    /// `w` 筛完一个都不剩时改用不筛的那份（首页库行的「未看优先」口径，默认关）。
    /// 墙上用户手选的「未观看」是严格筛，不带这个开关。
    watch_fallback: bool,
    hdr: Option<bool>,
    rating: Option<f64>,
    identity: Option<String>,
    pub sort: String,
    desc: bool,
}

impl LibraryFilter {
    pub fn parse(query: &HashMap<String, String>) -> Result<Self, String> {
        let mut lists: HashMap<String, Vec<String>> = HashMap::new();
        for key in ["g", "c", "d", "rt", "lang", "res", "stock"] {
            let raw = query
                .get(key)
                .or_else(|| (key == "res").then(|| query.get("resolutions")).flatten());
            lists.insert(
                key.to_string(),
                raw.map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            );
        }
        let hdr = query
            .get("hdr")
            .map(|s| s.parse::<bool>())
            .transpose()
            .map_err(|_| "hdr must be true or false")?;
        let rating = query
            .get("rating_gte")
            .map(|s| s.parse::<f64>())
            .transpose()
            .map_err(|_| "rating_gte must be numeric")?;
        if rating.is_some_and(|v| !v.is_finite() || !(0.0..=10.0).contains(&v)) {
            return Err("rating_gte must be between 0 and 10".into());
        }
        let watch = query.get("w").or_else(|| query.get("watch")).cloned();
        if watch.as_deref().is_some_and(|w| {
            ![
                "favorite",
                "seen",
                "watching",
                "played",
                "watched",
                "unwatched",
                "unplayed",
                "unseen",
            ]
            .contains(&w)
        }) {
            return Err("invalid watch filter".into());
        }
        let watch_fallback = query
            .get("w_fallback")
            .map(|s| s.parse::<bool>())
            .transpose()
            .map_err(|_| "w_fallback must be true or false")?
            .unwrap_or(false);
        let requested = query.get("sort").cloned().unwrap_or_else(|| "title".into());
        // Every key the wall can ask for must stay reachable; an unrecognised one
        // falls back to the stable title order instead of failing the request.
        let sort = if SORTS.contains(&requested.as_str()) {
            requested
        } else {
            tracing::warn!(sort = %requested, "未知的 Library 排序，回退到标题顺序");
            "title".into()
        };
        for (key, allowed) in [
            ("d", &["2020s", "2010s", "2000s", "1990s", "earlier"][..]),
            ("rt", &["lte60", "60to90", "90to120", "gt120"][..]),
            ("stock", &["missing", "unscraped"][..]),
        ] {
            if lists[key].iter().any(|v| !allowed.contains(&v.as_str())) {
                return Err(format!("invalid {key} filter"));
            }
        }
        let desc = match query.get("order").map(|s| s.as_str()) {
            Some("desc") => true,
            Some("asc") => false,
            _ => matches!(
                sort.as_str(),
                "added_at" | "release_date" | "rating" | "size" | "last_played"
            ),
        };
        Ok(Self {
            lists,
            hdr,
            rating,
            watch,
            watch_fallback,
            sort,
            identity: query.get("identity").cloned(),
            desc,
        })
    }

    fn any(&self, key: &str, mut predicate: impl FnMut(&str) -> bool) -> bool {
        self.lists[key].is_empty() || self.lists[key].iter().any(|value| predicate(value))
    }

    /// 观看状态那一档单独拿得出来：`w_fallback` 要在「除它以外都通过」的名单上
    /// 再决定是严格筛还是未看优先（见 `select`）。两者合起来才是完整的筛选。
    pub fn matches_watch(&self, item: &LibrarySelection) -> bool {
        match self.watch.as_deref() {
            Some("favorite") => item.favorite,
            Some("seen") => item.seen,
            Some("watching") => item.seen && !item.played,
            Some("played" | "watched") => item.played,
            Some("unwatched" | "unplayed" | "unseen") => !item.seen,
            _ => true,
        }
    }

    /// 请求里带没带观看状态那一档。
    pub fn has_watch_filter(&self) -> bool {
        self.watch.is_some()
    }

    /// 未看优先：`w` 一项都没留下时改用不筛的那份（`w_fallback=true`；
    /// 没给 `w` 时这个开关没有意义，恒 false）。
    pub fn prefers_unwatched(&self) -> bool {
        self.watch.is_some() && self.watch_fallback
    }

    pub fn matches_ignoring_watch(&self, item: &LibrarySelection) -> bool {
        let meta = &item.metadata;
        let provisional = item.media.tmdb_id.is_none()
            && item.media.douban_id.is_none()
            && item.media.tvdb_id.is_none()
            && item.media.bangumi_id.is_none()
            && item.media.anilist_id.is_none();
        let identity = match self.identity.as_deref() {
            Some("provisional") => provisional,
            Some("confirmed") => !provisional,
            _ => true,
        };
        identity
            && self.any("g", |g| meta.genres.iter().any(|v| v == g))
            && self.any("c", |c| {
                meta.countries.iter().any(|v| v.eq_ignore_ascii_case(c))
            })
            && self.any("lang", |lang| {
                meta.language
                    .as_deref()
                    .is_some_and(|v| v.eq_ignore_ascii_case(lang))
            })
            && self.any("d", |d| {
                item.media.year.is_some_and(|y| match d {
                    "earlier" => y < 1990,
                    _ => d
                        .strip_suffix('s')
                        .and_then(|d| d.parse::<u16>().ok())
                        .is_some_and(|start| y >= start && y < start + 10),
                })
            })
            && self.any("rt", |rt| {
                meta.runtime.is_some_and(|r| match rt {
                    "lte60" => r <= 60,
                    "60to90" => r > 60 && r <= 90,
                    "90to120" => r > 90 && r <= 120,
                    "gt120" => r > 120,
                    _ => false,
                })
            })
            && self.any("res", |res| {
                item.rows
                    .iter()
                    .any(|r| r.resolution.as_deref() == Some(res))
            })
            && self.any("stock", |stock| match stock {
                "missing" => item.missing,
                "unscraped" => !meta.scraped,
                _ => false,
            })
            && self.hdr.is_none_or(|want| {
                item.rows
                    .iter()
                    .any(|r| r.hdr.as_deref().is_some_and(|v| !v.is_empty()))
                    == want
            })
            && self
                .rating
                .is_none_or(|min| meta.rating.is_some_and(|rating| rating >= min))
    }

    /// Watch state is a per-unit store read; only requests that depend on it pay
    /// that cost, so ordinary wall browsing keeps its previous profile.
    pub fn needs_watch_state(&self) -> bool {
        self.watch.is_some() || self.sort == "last_played"
    }

    pub fn compare(&self, a: &LibrarySelection, b: &LibrarySelection) -> Ordering {
        let primary = match self.sort.as_str() {
            "added_at" => a.added_at.cmp(&b.added_at),
            "release_date" | "release_date_asc" => release_date(a).cmp(&release_date(b)),
            "rating" => a
                .metadata
                .rating
                .unwrap_or(-1.0)
                .total_cmp(&b.metadata.rating.unwrap_or(-1.0)),
            "runtime" => a
                .metadata
                .runtime
                .unwrap_or(0)
                .cmp(&b.metadata.runtime.unwrap_or(0)),
            "size" => a.size_bytes.unwrap_or(0).cmp(&b.size_bytes.unwrap_or(0)),
            "last_played" => a
                .last_played_at
                .unwrap_or(0)
                .cmp(&b.last_played_at.unwrap_or(0)),
            "resolution" => resolution_rank(best_resolution(&a.rows).unwrap_or(""))
                .cmp(&resolution_rank(best_resolution(&b.rows).unwrap_or(""))),
            // 稳定的伪随机序：同一 Media 每次落同一位置，翻页/EOF 才不会重复或漏项。
            "random" => shuffle_key(a).cmp(&shuffle_key(b)),
            _ => a.media.title.cmp(&b.media.title),
        };
        let primary = if self.desc {
            primary.reverse()
        } else {
            primary
        };
        primary.then_with(|| a.media.id.to_string().cmp(&b.media.id.to_string()))
    }
}

/// 未知或系统临时档（`probing`）的排序是视频探测状态，只有 ApiState 能回答，
/// 这里按标题稳定回退——不崩、不重复、不静默改变名单，只是顺序退回默认。
const SORTS: &[&str] = &[
    "title",
    "added_at",
    "release_date",
    "release_date_asc",
    "rating",
    "runtime",
    "size",
    "last_played",
    "resolution",
    "probing",
    "random",
];

fn shuffle_key(item: &LibrarySelection) -> u64 {
    // FNV-1a over the stable Media id.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in item.media.id.to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn release_date(item: &LibrarySelection) -> String {
    item.metadata.release_date.clone().unwrap_or_else(|| {
        item.media
            .year
            .map(|y| format!("{y:04}-01-01"))
            .unwrap_or_default()
    })
}
