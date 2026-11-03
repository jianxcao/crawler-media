//! Offline season hints use exact producer keys in configured locale, then neutral.
use crate::store::StoreError;
use domain::{Coverage, Media, MediaId, Subscribe, SubscribeFacts};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(super) type SeasonCounts = HashMap<(MediaId, u32), u32>;
pub(super) fn load<'a>(
    store: &crate::Store,
    media: impl Iterator<Item = &'a Media>,
) -> Result<SeasonCounts, StoreError> {
    let media: HashMap<_, _> = media
        .filter_map(|m| m.tmdb_id.as_deref().map(|id| (m.id, id)))
        .collect();
    if media.is_empty() {
        return Ok(HashMap::new());
    }
    let configured = store.metadata_language()?.language;
    let language = if configured.is_empty() {
        "zh-CN"
    } else {
        &configured
    };
    let mut candidates = HashMap::new();
    let mut keys = HashSet::new();
    for (id, tmdb) in &media {
        let ordered: Vec<_> = [language, ""]
            .into_iter()
            .flat_map(|locale| {
                media::tmdb_keys::tv_details_paths(tmdb)
                    .map(|path| media::tmdb_keys::request_key(&path, locale))
            })
            .collect();
        keys.extend(ordered.iter().cloned());
        candidates.insert(*id, ordered);
    }
    // Expired responses remain usable offline hints, as in Tmdb::cached_fetch.
    // No readiness/TTL assertion is inferred from Store's raw cache read.
    let bodies = store.catalog_cache_for_keys("tmdb", &keys.into_iter().collect::<Vec<_>>())?;
    let mut out = HashMap::new();
    for (id, ordered) in candidates {
        for key in ordered {
            let Some(body) = bodies.get(&key) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(body) else {
                tracing::warn!(media_id=%id, cache_key=%key, "invalid cached TMDB details JSON; trying another exact candidate");
                continue;
            };
            if value.get("id").map(Value::to_string).as_deref() != media.get(&id).copied() {
                continue;
            }
            let Some(seasons) = value.get("seasons").and_then(Value::as_array) else {
                continue;
            };
            for season in seasons {
                let number = season
                    .get("season_number")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok());
                let count = season
                    .get("episode_count")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0);
                if let (Some(number), Some(count)) = (number, count) {
                    out.entry((id, number))
                        .or_insert_with(|| u32::try_from(count).unwrap_or(u32::MAX));
                }
            }
        }
    }
    Ok(out)
}

pub(super) fn calculate(
    subscribe: &Subscribe,
    media: &Media,
    facts: &SubscribeFacts,
    counts: &SeasonCounts,
) -> (i64, i64) {
    match subscribe.coverage {
        Coverage::Movie => (1, i64::from(facts.movie().is_some())),
        Coverage::Tv {
            episode_to: Some(_),
            ..
        } => {
            let units = subscribe.coverage.units();
            (
                units.len() as i64,
                units
                    .iter()
                    .filter(|(s, e)| facts.get(*s, *e).is_some())
                    .count() as i64,
            )
        }
        Coverage::Tv {
            season,
            episode_from,
            episode_to: None,
        } => {
            if let Some(count) = counts.get(&(media.id, season)) {
                let to = (*count)
                    .max(episode_from)
                    .min(episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1)));
                let total = i64::from(to.saturating_sub(episode_from)) + 1;
                let imported = (episode_from..=to)
                    .filter(|ep| facts.get(Some(season), Some(*ep)).is_some())
                    .count() as i64;
                (total, imported)
            } else {
                let imported = facts
                    .entries()
                    .filter(|((s, e), _)| *s == Some(season) && e.is_some())
                    .count() as i64;
                (imported.max(1), imported)
            }
        }
    }
}
