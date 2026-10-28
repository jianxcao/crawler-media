use super::{CatalogCacheRow, MediaCacheEntry};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

type Entries = HashMap<String, MediaCacheEntry>;
type SeenKeys = HashSet<(String, String)>;

pub(super) fn aggregate_media_entries(rows: Vec<CatalogCacheRow>) -> Vec<MediaCacheEntry> {
    let mut map = Entries::new();
    let mut seen = SeenKeys::new();
    for row in rows {
        let body = row.body.trim();
        if body.starts_with("<!DOCTYPE") || body.starts_with("<html") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        match row.source.as_str() {
            "tmdb" => {
                if !tmdb_detail(&row, &value, &mut map, &mut seen) {
                    tmdb_results(&row, &value, &mut map, &mut seen);
                }
            }
            "douban" => douban(&row, &value, &mut map, &mut seen),
            "anilist" => anilist(&row, &value, &mut map, &mut seen),
            "bangumi" => bangumi(&row, &value, &mut map, &mut seen),
            _ => {}
        }
    }
    let mut list: Vec<_> = map
        .into_values()
        .filter(|entry| !entry.title.is_empty())
        .collect();
    list.sort_by_key(|entry| std::cmp::Reverse(entry.fetched_at));
    list
}

fn parse_year(date: Option<&str>) -> Option<u32> {
    date?.get(..4)?.parse().ok()
}

fn record_key(entry: &mut MediaCacheEntry, key: &str, seen: &mut SeenKeys) {
    if seen.insert((entry.id.clone(), key.to_string())) {
        entry.cached_keys.push(key.to_string());
    }
}

fn tmdb_detail(row: &CatalogCacheRow, val: &Value, map: &mut Entries, seen: &mut SeenKeys) -> bool {
    let is_movie = row.cache_key.starts_with("/movie/");
    let is_tv = row.cache_key.starts_with("/tv/");
    if (is_movie || is_tv)
        && !row.cache_key.contains("/now_playing")
        && !row.cache_key.contains("/popular")
        && !row.cache_key.contains("/upcoming")
        && !row.cache_key.contains("/top_rated")
        && !row.cache_key.contains("/airing_today")
        && !row.cache_key.contains("/on_the_air")
    {
        let parts: Vec<&str> = row
            .cache_key
            .split('?')
            .next()
            .unwrap_or("")
            .split('/')
            .collect();
        if parts.len() >= 3 {
            let source_id = parts[2].to_string();
            if source_id.chars().all(|c| c.is_ascii_digit()) {
                let kind = if is_movie { "movie" } else { "tv" };
                let entry_key = format!("tmdb:{kind}:{source_id}");

                let title = val
                    .get("title")
                    .or_else(|| val.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let original_title = val
                    .get("original_title")
                    .or_else(|| val.get("original_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let year = parse_year(
                    val.get("release_date")
                        .or_else(|| val.get("first_air_date"))
                        .and_then(Value::as_str),
                );
                let poster = val
                    .get("poster_path")
                    .and_then(Value::as_str)
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"));
                let backdrop = val
                    .get("backdrop_path")
                    .and_then(Value::as_str)
                    .map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"));
                let overview = val
                    .get("overview")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let rating = val.get("vote_average").and_then(Value::as_f64);
                let genres = val
                    .get("genres")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|g| {
                                g.get("name").and_then(Value::as_str).map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                    id: format!("tmdb_{kind}_{source_id}"),
                    source: "tmdb".into(),
                    source_id: source_id.clone(),
                    title: title.to_string(),
                    original_title,
                    kind: kind.to_string(),
                    year,
                    poster_url: poster.clone(),
                    backdrop_url: backdrop.clone(),
                    overview: overview.clone(),
                    rating,
                    genres,
                    fetched_at: row.fetched_at,
                    expires_at: row.expires_at,
                    cached_keys: Vec::new(),
                });

                if !title.is_empty()
                    && (entry.title.is_empty() || row.cache_key.contains("language=zh-CN"))
                {
                    entry.title = title.to_string();
                }
                if entry.poster_url.is_none() && poster.is_some() {
                    entry.poster_url = poster;
                }
                if entry
                    .overview
                    .as_ref()
                    .map(|o| o.is_empty())
                    .unwrap_or(true)
                    && overview.is_some()
                {
                    entry.overview = overview;
                }
                if entry.year.is_none() && year.is_some() {
                    entry.year = year;
                }
                record_key(entry, &row.cache_key, seen);
                return true;
            }
        }
    }

    false
}

fn tmdb_results(row: &CatalogCacheRow, val: &Value, map: &mut Entries, seen: &mut SeenKeys) {
    let results = val.get("results").and_then(Value::as_array);
    if let Some(list) = results {
        for item in list {
            if let Some(id) = item.get("id").and_then(Value::as_i64) {
                let id_str = id.to_string();
                let is_tv = item.get("first_air_date").is_some()
                    || item.get("name").is_some()
                    || row.cache_key.contains("/tv");
                let kind = if is_tv { "tv" } else { "movie" };
                let entry_key = format!("tmdb:{kind}:{id_str}");

                let title = item
                    .get("title")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let original_title = item
                    .get("original_title")
                    .or_else(|| item.get("original_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let year = parse_year(
                    item.get("release_date")
                        .or_else(|| item.get("first_air_date"))
                        .and_then(Value::as_str),
                );
                let poster = item
                    .get("poster_path")
                    .and_then(Value::as_str)
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"));
                let backdrop = item
                    .get("backdrop_path")
                    .and_then(Value::as_str)
                    .map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"));
                let overview = item
                    .get("overview")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let rating = item.get("vote_average").and_then(Value::as_f64);

                let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                    id: format!("tmdb_{kind}_{id_str}"),
                    source: "tmdb".into(),
                    source_id: id_str,
                    title: title.to_string(),
                    original_title,
                    kind: kind.to_string(),
                    year,
                    poster_url: poster.clone(),
                    backdrop_url: backdrop.clone(),
                    overview: overview.clone(),
                    rating,
                    genres: Vec::new(),
                    fetched_at: row.fetched_at,
                    expires_at: row.expires_at,
                    cached_keys: Vec::new(),
                });

                if !title.is_empty()
                    && (entry.title.is_empty() || row.cache_key.contains("language=zh-CN"))
                {
                    entry.title = title.to_string();
                }
                if entry.poster_url.is_none() && poster.is_some() {
                    entry.poster_url = poster;
                }
                if entry.year.is_none() && year.is_some() {
                    entry.year = year;
                }
                record_key(entry, &row.cache_key, seen);
            }
        }
        return;
    }
}

fn douban(row: &CatalogCacheRow, val: &Value, map: &mut Entries, seen: &mut SeenKeys) {
    let subjects = val.get("subjects").and_then(Value::as_array);
    if let Some(list) = subjects {
        for item in list {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                let title = item.get("title").and_then(Value::as_str).unwrap_or("");
                let poster = item
                    .get("cover")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let rate = item
                    .get("rate")
                    .and_then(Value::as_str)
                    .and_then(|r| r.parse::<f64>().ok());
                let kind = if row.cache_key.contains("type=tv") {
                    "tv"
                } else {
                    "movie"
                };
                let entry_key = format!("douban:{kind}:{id}");

                let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                    id: format!("douban_{kind}_{id}"),
                    source: "douban".into(),
                    source_id: id.to_string(),
                    title: title.to_string(),
                    original_title: None,
                    kind: kind.to_string(),
                    year: None,
                    poster_url: poster.clone(),
                    backdrop_url: None,
                    overview: None,
                    rating: rate,
                    genres: Vec::new(),
                    fetched_at: row.fetched_at,
                    expires_at: row.expires_at,
                    cached_keys: Vec::new(),
                });
                record_key(entry, &row.cache_key, seen);
            }
        }
        return;
    }
}

fn anilist(row: &CatalogCacheRow, val: &Value, map: &mut Entries, seen: &mut SeenKeys) {
    let media_list = val.pointer("/data/Page/media").and_then(Value::as_array);
    if let Some(list) = media_list {
        for item in list {
            if let Some(id) = item.get("id").and_then(Value::as_i64) {
                let id_str = id.to_string();
                let title = item
                    .pointer("/title/native")
                    .or_else(|| item.pointer("/title/romaji"))
                    .or_else(|| item.pointer("/title/english"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let poster = item
                    .pointer("/coverImage/large")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let banner = item
                    .get("bannerImage")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let overview = item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let format = item.get("format").and_then(Value::as_str).unwrap_or("TV");
                let kind = if format == "MOVIE" { "movie" } else { "tv" };
                let entry_key = format!("anilist:{kind}:{id_str}");

                let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                    id: format!("anilist_{kind}_{id_str}"),
                    source: "anilist".into(),
                    source_id: id_str,
                    title: title.to_string(),
                    original_title: item
                        .pointer("/title/romaji")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    kind: kind.to_string(),
                    year: item
                        .pointer("/startDate/year")
                        .and_then(Value::as_i64)
                        .map(|y| y as u32),
                    poster_url: poster,
                    backdrop_url: banner,
                    overview,
                    rating: item
                        .get("averageScore")
                        .and_then(Value::as_f64)
                        .map(|s| s / 10.0),
                    genres: item
                        .get("genres")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|g| g.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    fetched_at: row.fetched_at,
                    expires_at: row.expires_at,
                    cached_keys: Vec::new(),
                });
                record_key(entry, &row.cache_key, seen);
            }
        }
        return;
    }
}

fn bangumi(row: &CatalogCacheRow, val: &Value, map: &mut Entries, seen: &mut SeenKeys) {
    let list = val.get("list").and_then(Value::as_array);
    if let Some(items) = list {
        for item in items {
            if let Some(id) = item.get("id").and_then(Value::as_i64) {
                let id_str = id.to_string();
                let title = item
                    .get("name_cn")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| item.get("name").and_then(Value::as_str))
                    .unwrap_or("");
                let poster = item
                    .pointer("/images/large")
                    .or_else(|| item.pointer("/images/common"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let year = parse_year(item.get("air_date").and_then(Value::as_str));
                let overview = item
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let rating = item.get("score").and_then(Value::as_f64);
                let kind = if row.cache_key.contains("type=6") {
                    "movie"
                } else {
                    "tv"
                };
                let entry_key = format!("bangumi:{kind}:{id_str}");

                let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                    id: format!("bangumi_{kind}_{id_str}"),
                    source: "bangumi".into(),
                    source_id: id_str,
                    title: title.to_string(),
                    original_title: item.get("name").and_then(Value::as_str).map(str::to_string),
                    kind: kind.to_string(),
                    year,
                    poster_url: poster,
                    backdrop_url: None,
                    overview,
                    rating,
                    genres: vec!["动画".into()],
                    fetched_at: row.fetched_at,
                    expires_at: row.expires_at,
                    cached_keys: Vec::new(),
                });
                record_key(entry, &row.cache_key, seen);
            }
        }
        return;
    }
}
