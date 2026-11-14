//! Subscribe schedule facts captured from adopted Library metadata, never origin.
use crate::scrape_store::ScrapeStoreExt;
use domain::{FileCoverage, Subscribe};

pub(super) fn adopted_episodes(
    store: &crate::Store,
    subscribe: &Subscribe,
    rows: &[FileCoverage],
) -> Result<Vec<media::SeasonEpisode>, store::StoreError> {
    let domain::Coverage::Tv { season, .. } = subscribe.coverage else {
        return Ok(Vec::new());
    };
    let library = subscribe
        .library_id
        .or_else(|| rows.first().map(|row| row.file.library_id));
    let locale = store
        .get_scrape_config()?
        .effective
        .primary_language()
        .to_string();
    Ok(store
        .adopted_episode_metadata(subscribe.media_id, season, library, &locale)?
        .into_iter()
        .map(|episode| media::SeasonEpisode {
            episode_number: episode.episode,
            air_date: episode.air_date,
        })
        .collect())
}
