use domain::{Site, Torrent};
use indexer::CategoryMapping;

use super::config::EffectiveBoostRule;
use crate::management::ApiState;

pub(super) fn is_site_eligible(
    state: &ApiState,
    site: &Site,
    rule: &EffectiveBoostRule,
    now: i64,
) -> bool {
    if let Some(ah) = rule.active_hours
        && !ah.is_active_at(now, Some(&site.timezone))
    {
        return false;
    }
    if let Some(target_ratio) = rule.target_site_ratio {
        match state.indexer.fetch_user_stats(site) {
            Ok(Some((uploaded, downloaded))) => {
                if downloaded == 0 {
                    if uploaded > 0 {
                        return false;
                    }
                } else if (uploaded as f64 / downloaded as f64) >= target_ratio {
                    return false;
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    site_id = %site.id,
                    error = %err,
                    "boost site skipped because user_stats lookup failed"
                );
                return false;
            }
        }
    }
    true
}

pub(super) fn candidate_matches(
    torrent: &Torrent,
    site: &Site,
    rule: &EffectiveBoostRule,
    cat_mapping: Option<&CategoryMapping>,
    now: i64,
) -> bool {
    if !rule.categories.is_empty() {
        let norm = torrent
            .category
            .as_deref()
            .and_then(|c| indexer::normalize_category(c, cat_mapping))
            .map(|c| c.to_ascii_lowercase());
        if !norm.is_some_and(|c| rule.categories.contains(&c)) {
            return false;
        }
    }
    if !rule.free_mode.matches(torrent.free, torrent.free_2x) {
        return false;
    }
    if rule.exclude_hr && torrent.hr == Some(true) {
        return false;
    }
    if let Some(seeders) = torrent.seeders {
        let s = i32::try_from(seeders).unwrap_or(i32::MAX);
        if s < rule.min_seeders || rule.max_seeders.is_some_and(|max_s| s > max_s) {
            return false;
        }
    }
    if let Some(min_l) = rule.min_leechers
        && torrent
            .leechers
            .is_none_or(|l| i32::try_from(l).unwrap_or(i32::MAX) < min_l)
    {
        return false;
    }
    if let Some(min_sz) = rule.min_size_bytes
        && torrent.size_bytes.is_none_or(|sz| sz < min_sz)
    {
        return false;
    }
    if let Some(max_sz) = rule.max_size_bytes
        && torrent.size_bytes.is_none_or(|sz| sz > max_sz)
    {
        return false;
    }
    if let Some(max_age_mins) = rule.max_pub_age_minutes {
        let Some(raw_time) = torrent.upload_time.as_deref() else {
            return false;
        };
        let Some(pub_ts) = indexer::parse_site_datetime(raw_time, &site.timezone) else {
            return false;
        };
        let max_age_secs = i64::try_from(max_age_mins)
            .unwrap_or(i64::MAX)
            .saturating_mul(60);
        if now.saturating_sub(pub_ts) > max_age_secs {
            return false;
        }
    }
    if let Some(re) = &rule.include_regex
        && !re.is_match(&torrent.title)
    {
        return false;
    }
    if let Some(re) = &rule.exclude_regex
        && re.is_match(&torrent.title)
    {
        return false;
    }
    true
}
