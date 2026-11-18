//! Search suggestions never adopt an identity; the existing pinned POST does.

use std::collections::HashSet;

use domain::{Media, MediaKind};
use serde_json::{Value, json};

use crate::catalog::Catalog;

pub(super) fn outcome(
    media: &Media,
    seed: &str,
    poster_url: Option<String>,
    catalog: &dyn Catalog,
) -> (Value, bool) {
    if has_identity(media) || media.kind == MediaKind::Video {
        return (
            json!({
                "media_item_id": media.id.to_string(), "tmdb_id": media.tmdb_id,
                "title": media.title, "year": media.year, "poster_url": poster_url,
                "source": "current", "same_as_current": true,
                "reason": null, "code": null, "candidates": [],
            }),
            false,
        );
    }
    let seed = seed.trim();
    if seed.is_empty() {
        return pending(Vec::new(), Some("unparsable"), "缺少可搜索的名称");
    }
    let catalog = catalog.source("tmdb").unwrap_or(catalog);
    let hits = match media.kind {
        MediaKind::Movie => catalog.search_movie(seed),
        MediaKind::Tv => catalog.search_tv_year(seed, media.year),
        MediaKind::Video => unreachable!(),
    };
    let hits = match hits {
        Ok(hits) => hits,
        Err(error) => {
            tracing::warn!(media_id = %media.id, query = seed, %error,
                "身份复核候选搜索暂不可达，保留当前身份");
            return pending(
                Vec::new(),
                Some("tmdb_unreachable"),
                "TMDB 暂不可达，请稍后重试",
            );
        }
    };
    let mut seen = HashSet::new();
    let candidates = hits
        .into_iter()
        .filter(|hit| hit.media.kind == media.kind)
        .filter(|hit| media.year.is_none_or(|year| hit.media.year == Some(year)))
        .filter_map(|hit| {
            let id = hit.media.tmdb_id.as_deref()?.parse::<u64>().ok()?;
            (id > 0 && seen.insert(id)).then(|| {
                json!({
                    "tmdb_id": id, "title": hit.media.title, "year": hit.media.year,
                    "episode_count": null, "reasons": ["类型与已知年份相符，需确认身份"],
                })
            })
        })
        .take(20)
        .collect::<Vec<_>>();
    match candidates.len() {
        0 => pending(candidates, Some("no_match"), "没有同类型及已知年份的候选"),
        1 => pending(candidates, None, "找到一个候选，请查看详情后确认"),
        _ => pending(
            candidates,
            Some("ambiguous"),
            "多个候选符合条件，请查看 TMDB ID 与详情后选择",
        ),
    }
}

fn has_identity(media: &Media) -> bool {
    [
        &media.tmdb_id,
        &media.douban_id,
        &media.tvdb_id,
        &media.bangumi_id,
        &media.anilist_id,
    ]
    .into_iter()
    .any(|id| id.as_deref().is_some_and(|id| !id.trim().is_empty()))
}

fn pending(candidates: Vec<Value>, code: Option<&str>, reason: &str) -> (Value, bool) {
    (
        json!({
            "media_item_id": null, "tmdb_id": null, "title": null, "year": null,
            "poster_url": null, "source": "resolved", "same_as_current": false,
            "reason": reason, "code": code, "candidates": candidates,
        }),
        code == Some("tmdb_unreachable"),
    )
}
