use std::collections::BTreeMap;
use serde_json::Value;

use crate::client::TmdbError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanartImage {
    pub url: String,
    pub lang: Option<String>,
    pub likes: u32,
    pub season: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FanartSet {
    pub logo: Option<FanartImage>,
    pub thumb: Option<FanartImage>,
    pub banner: Option<FanartImage>,
    pub season_posters: Vec<FanartImage>,
    pub season_thumbs: Vec<FanartImage>,
    pub season_banners: Vec<FanartImage>,
}

fn parse_likes(val: Option<&Value>) -> u32 {
    match val {
        Some(Value::String(s)) => s.parse::<u32>().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
        _ => 0,
    }
}

fn parse_images_from_value(val: Option<&Value>) -> Vec<FanartImage> {
    let mut result = Vec::new();
    let Some(Value::Array(items)) = val else {
        return result;
    };

    for item in items {
        let Some(url) = item.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        let lang = item
            .get("lang")
            .and_then(|l| l.as_str())
            .map(|s| s.to_string());
        let likes = parse_likes(item.get("likes"));
        let season = item
            .get("season")
            .and_then(|s| {
                if let Some(str_val) = s.as_str() {
                    str_val.parse::<u32>().ok()
                } else if let Some(num_val) = s.as_u64() {
                    Some(num_val as u32)
                } else {
                    None
                }
            });

        result.push(FanartImage {
            url: url.to_string(),
            lang,
            likes,
            season,
        });
    }

    result
}

fn pick_best_image(candidates: &[FanartImage], languages: &[&str]) -> Option<FanartImage> {
    if candidates.is_empty() {
        return None;
    }

    let mut search_langs: Vec<&str> = languages.to_vec();
    if !search_langs.contains(&"zh") {
        search_langs.push("zh");
    }
    if !search_langs.contains(&"en") {
        search_langs.push("en");
    }

    for wanted in search_langs {
        let mut matched: Vec<&FanartImage> = candidates
            .iter()
            .filter(|img| {
                let lang_str = img.lang.as_deref().unwrap_or("");
                if wanted.is_empty() {
                    lang_str.is_empty() || lang_str == "00"
                } else {
                    lang_str == wanted
                }
            })
            .collect();

        if !matched.is_empty() {
            matched.sort_by(|a, b| b.likes.cmp(&a.likes));
            return Some((*matched[0]).clone());
        }
    }

    let mut all: Vec<&FanartImage> = candidates.iter().collect();
    all.sort_by(|a, b| b.likes.cmp(&a.likes));
    Some((*all[0]).clone())
}

fn pick_first_slot(root: &Value, keys: &[&str], languages: &[&str]) -> Option<FanartImage> {
    for key in keys {
        let images = parse_images_from_value(root.get(*key));
        if let Some(picked) = pick_best_image(&images, languages) {
            return Some(picked);
        }
    }
    None
}

fn pick_season_images(root: &Value, key: &str, languages: &[&str]) -> Vec<FanartImage> {
    let images = parse_images_from_value(root.get(key));
    let mut by_season: BTreeMap<u32, Vec<FanartImage>> = BTreeMap::new();
    for img in images {
        if let Some(season_num) = img.season {
            by_season.entry(season_num).or_default().push(img);
        }
    }

    let mut result = Vec::new();
    for (_season, candidates) in by_season {
        if let Some(picked) = pick_best_image(&candidates, languages) {
            result.push(picked);
        }
    }
    result
}

pub fn parse_fanart(body: &str, languages: &[&str]) -> Result<FanartSet, TmdbError> {
    let value: Value = serde_json::from_str(body)?;
    if value.get("status").and_then(|s| s.as_str()) == Some("error") {
        return Ok(FanartSet::default());
    }

    let logo = pick_first_slot(&value, &["hdtvlogo", "hdmovielogo", "movielogo"], languages);
    let thumb = pick_first_slot(&value, &["tvthumb", "moviethumb"], languages);
    let banner = pick_first_slot(&value, &["tvbanner", "moviebanner"], languages);

    let season_posters = pick_season_images(&value, "seasonposter", languages);
    let season_thumbs = pick_season_images(&value, "seasonthumb", languages);
    let season_banners = pick_season_images(&value, "seasonbanner", languages);

    Ok(FanartSet {
        logo,
        thumb,
        banner,
        season_posters,
        season_thumbs,
        season_banners,
    })
}
