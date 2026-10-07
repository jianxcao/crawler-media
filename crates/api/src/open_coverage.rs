use std::collections::HashMap;

use domain::{Coverage, Media, Subscribe};

use crate::store::{PendingDownload, WantedHistory};

/// Materialize a finite view of an open TV Subscribe: known episodes plus the
/// next episode to keep searching for. Other coverage uses its declared range.
pub(crate) fn units(
    subscribe: &Subscribe,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
    pending: &[(i32, PendingDownload)],
    history: &HashMap<(Option<u32>, Option<u32>), WantedHistory>,
    observed: impl IntoIterator<Item = u32>,
) -> Vec<(Option<u32>, Option<u32>)> {
    let Coverage::Tv {
        season,
        episode_from,
        episode_to: None,
    } = subscribe.coverage
    else {
        return subscribe.coverage.units();
    };
    let mut highest = episode_from;
    for ((s, ep), _) in facts.entries() {
        if s == Some(season) {
            highest = highest.max(ep.unwrap_or(0));
        }
    }
    for ((s, ep), row) in history {
        if *s == Some(season) && (row.grabbed_at.is_some() || row.imported_at.is_some()) {
            highest = highest.max(ep.unwrap_or(0));
        }
    }
    for (_, item) in pending {
        let release = item
            .release_override
            .clone()
            .unwrap_or_else(|| release::parse(&item.torrent.title));
        if subscribe::candidate_matches_subscribe(subscribe, media, &release)
            && release.season == Some(season)
        {
            highest = highest.max(release.episode_to.or(release.episode).unwrap_or(0));
        }
    }
    for ep in observed {
        highest = highest.max(ep);
    }
    let to = highest
        .saturating_add(1)
        .min(episode_from.saturating_add(domain::Coverage::MAX_EPISODES.saturating_sub(1)));
    (episode_from..=to)
        .map(|ep| (Some(season), Some(ep)))
        .collect()
}
