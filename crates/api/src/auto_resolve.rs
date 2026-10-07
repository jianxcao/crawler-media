//! Auto-resolve media metadata against TMDB/Catalog during library scans.
//! Inspired by MovieClaw & MoviePilot two-stage identification.

use crate::management::ApiState;
use domain::{Media, MediaKind};

/// Automatically look up TMDB metadata for a media item if it lacks external IDs.
/// If an exact or high-confidence match is found:
/// 1. Updates `media` in the database with `tmdb_id`, `year`, `original_title`
/// 2. Downloads and writes the official poster beside the preferred file
pub fn auto_resolve_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
) -> Option<Media> {
    if media.tmdb_id.is_some() || media.douban_id.is_some() {
        return Some(media.clone());
    }

    let title = media.title.trim();
    if title.is_empty() {
        return None;
    }

    tracing::info!(
        title = %title,
        kind = ?media.kind,
        "扫描时自动解析媒体元数据（TMDB/目录）"
    );

    let hits = match media.kind {
        MediaKind::Movie => state.catalog.search_movie(title).ok(),
        MediaKind::Tv => state.catalog.search_tv(title).ok(),
        MediaKind::Video => None,
    }?;

    let Some(best_hit) = find_best_catalog_match(title, media.year, &hits) else {
        tracing::debug!(title = %title, "扫描时未找到可靠的 TMDB 匹配");
        return None;
    };

    let tmdb_id = best_hit.media.tmdb_id.clone()?;
    tracing::info!(
        title = %title,
        matched_title = %best_hit.media.title,
        tmdb_id = %tmdb_id,
        year = ?best_hit.media.year,
        "成功匹配 TMDB 媒体"
    );

    let mut updated = media.clone();
    if updated.tmdb_id.is_none() {
        updated.tmdb_id = best_hit.media.tmdb_id.clone();
    }
    if updated.douban_id.is_none() {
        updated.douban_id = best_hit.media.douban_id.clone();
    }
    if updated.year.is_none() {
        updated.year = best_hit.media.year;
    }
    if updated.original_title.is_none() {
        updated.original_title = best_hit.media.original_title.clone();
    }

    // Persist updated TMDB ID / Douban ID to the database
    {
        let store = state.store.lock();
        if let Err(e) = store.update_media(&updated) {
            tracing::warn!(error = %e, "更新媒体 TMDB/豆瓣 ID 失败");
        }
    }

    // Attach official poster & backdrop
    let _ = crate::poster_fetch::attach_poster(state, &updated, sample_path);
    let _ = crate::poster_fetch::attach_backdrop(state, &updated, sample_path);

    Some(updated)
}

fn find_best_catalog_match<'a>(
    title: &str,
    year: Option<u16>,
    hits: &'a [media::CatalogHit],
) -> Option<&'a media::CatalogHit> {
    if hits.is_empty() {
        return None;
    }

    let norm_target = normalize_title(title);
    let eligible: Vec<_> = hits
        .iter()
        .filter(|hit| year.is_none_or(|target| hit.media.year.is_none_or(|found| found == target)))
        .collect();

    // 1. Exact title match + year match
    if let Some(target_year) = year {
        for hit in &eligible {
            if hit.media.year == Some(target_year)
                && normalize_title(&hit.media.title) == norm_target
            {
                return Some(hit);
            }
        }
    }

    // 2. Exact title match (regardless of year)
    for hit in &eligible {
        if normalize_title(&hit.media.title) == norm_target {
            return Some(hit);
        }
    }

    // 3. If only 1 hit returned and title contains target or target contains title
    if eligible.len() == 1 {
        let hit_title = normalize_title(&eligible[0].media.title);
        if hit_title.contains(&norm_target) || norm_target.contains(&hit_title) {
            return Some(eligible[0]);
        }
    }

    None
}

fn normalize_title(t: &str) -> String {
    t.chars()
        .filter(|c| {
            !c.is_whitespace() && *c != '.' && *c != '-' && *c != '_' && *c != ':' && *c != '：'
        })
        .collect::<String>()
        .to_lowercase()
}
