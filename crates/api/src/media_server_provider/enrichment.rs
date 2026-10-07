use super::*;

impl ApiServerProvider {
    pub(super) async fn enrich_single_item(
        &self,
        mut snapshot: MediaItemSnapshot,
    ) -> MediaItemSnapshot {
        if !snapshot.is_series {
            crate::http::library::ensure_probe_enqueued(&self.state, &snapshot.row);
        }
        let Some(tmdb_id) = snapshot.media.tmdb_id.clone() else {
            return snapshot;
        };
        let state = self.state.clone();
        let media_kind = snapshot.media.kind;
        let episode_key = (!snapshot.is_series && media_kind == MediaKind::Tv)
            .then(|| snapshot.row.season.zip(snapshot.row.episode))
            .flatten();
        let tmdb_lookup_id = tmdb_id.clone();
        let lookup = tokio::task::spawn_blocking(
            move || -> Result<Option<(media::ItemMeta, Option<media::EpisodeMeta>)>, String> {
                let Some(metadata) = crate::scrape_metadata::fetch_tmdb_metadata(
                    &state,
                    media_kind,
                    &tmdb_lookup_id,
                )?
                else {
                    return Ok(None);
                };
                let episode = episode_key.and_then(|(season, episode)| {
                    catalog_episode(&state, &tmdb_lookup_id, season, episode)
                });
                Ok(Some((metadata, episode)))
            },
        )
        .await;
        match lookup {
            Ok(Ok(Some((metadata, episode)))) => {
                snapshot.metadata = merge_catalog_metadata(
                    snapshot.metadata,
                    &snapshot.media,
                    &metadata,
                    episode.as_ref(),
                );
            }
            Ok(Ok(None)) => {
                tracing::debug!(%tmdb_id, "catalog returned no metadata for Jellyfin item")
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, %tmdb_id, "failed to load catalog metadata for Jellyfin item")
            }
            Err(error) => tracing::warn!(%error, %tmdb_id, "Jellyfin catalog metadata task failed"),
        }
        snapshot
    }
}

fn catalog_episode(
    state: &ApiState,
    tmdb_lookup_id: &str,
    season: u32,
    episode: u32,
) -> Option<media::EpisodeMeta> {
    let language = crate::scrape_metadata::preferred_language(&state);
    let translated = match state
        .catalog
        .season_details_lang(&tmdb_lookup_id, season, &language)
    {
        Ok(episodes) if !episodes.is_empty() => episodes,
        Ok(_) => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to load translated episode metadata");
            Vec::new()
        }
    };
    let episodes = if translated.is_empty() {
        match state.catalog.season_details(&tmdb_lookup_id, season) {
            Ok(episodes) => episodes,
            Err(error) => {
                tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to load episode metadata");
                Vec::new()
            }
        }
    } else {
        translated
    };
    episodes
        .into_iter()
        .find(|metadata| metadata.episode_number == episode)
}
