//! TV episode 5-state availability enrichment for Discover and Library detail routes.

use super::*;

pub(super) fn season_detail_blocking(
    state: &ApiState,
    viewer: Option<domain::UserId>,
    id: &str,
    season: u32,
) -> Response {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let tmdb_id = match tmdb_id_for(state, catalog, domain::MediaKind::Tv, id) {
        Ok(Some(tid)) => tid,
        Ok(None) => return err(StatusCode::NOT_FOUND, "media.missing", "条目不存在"),
        Err(response) => return response,
    };
    let meta_episodes = match catalog.cached_season_details(&tmdb_id, season) {
        Ok(Some(rows)) => rows,
        Ok(None) | Err(_) => Vec::new(),
    };
    let cached_episodes = match catalog.cached_season_episodes(&tmdb_id, season) {
        Ok(episodes) => episodes,
        Err(error) => return err(StatusCode::BAD_GATEWAY, "media.upstream", &error),
    };
    let (media_ids, panorama) = panorama_for_tmdb_tv(state, viewer, &tmdb_id);
    if let Some(episodes) = cached_episodes {
        let items: Vec<Value> = episodes
            .into_iter()
            .map(|ep| {
                let ep_num = ep.episode_number;
                let meta = meta_episodes.iter().find(|m| m.episode_number == ep_num);
                season_episode_json(
                    season,
                    ep_num,
                    meta.and_then(|m| m.name.clone()),
                    meta.and_then(|m| m.overview.clone()),
                    meta.and_then(|m| m.still_path.as_deref().map(episode_still_url)),
                    ep.air_date,
                    &panorama,
                )
            })
            .collect();
        return ok(json!({ "episodes": items })).into_response();
    }
    if let Some(&media_id) = media_ids.first()
        && let Ok(local_eps) = state
            .store
            .adopted_episode_metadata(media_id, season, None, "")
        && !local_eps.is_empty()
    {
        let items: Vec<Value> = local_eps
            .into_iter()
            .map(|ep| {
                season_episode_json(
                    season,
                    ep.episode,
                    ep.title,
                    ep.overview,
                    ep.still_url.as_deref().map(episode_still_url),
                    ep.air_date,
                    &panorama,
                )
            })
            .collect();
        return ok(json!({ "episodes": items })).into_response();
    }
    ok(json!({ "episodes": [] })).into_response()
}

fn season_episode_json(
    season: u32,
    episode_number: u32,
    name: Option<String>,
    overview: Option<String>,
    still_url: Option<String>,
    air_date: Option<String>,
    panorama: &store::EpisodePanorama,
) -> Value {
    let availability = panorama.episode_availability(season, episode_number, air_date.as_deref());
    let owned = availability.status == store::EpisodeAvailabilityStatus::InLibrary;
    let owned_in = availability.owned_in.as_ref();
    json!({
        "episode_number": episode_number,
        "name": name,
        "overview": overview,
        "still_url": still_url,
        "air_date": air_date,
        "status": availability.status.as_str(),
        "owned": owned,
        "library_id": owned_in.map(|o| o.library_id.to_string()),
        "media_item_id": owned_in.map(|o| o.media_item_id.to_string()),
        "resolution": owned_in.and_then(|o| o.resolution.clone()),
    })
}

pub(super) fn panorama_for_tmdb_tv(
    state: &ApiState,
    user_id: Option<domain::UserId>,
    tmdb_id: &str,
) -> (Vec<domain::MediaId>, store::EpisodePanorama) {
    let result = state.store.with_read_snapshot(
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        crate::http::store_wait::STORE_READ_WAIT,
        || {
            let store = state.store.as_ref();
            let is_admin = user_id
                .and_then(|uid| store.user_role(uid).ok())
                .is_some_and(|role| role == "admin");
            let visible_libs: Vec<domain::LibraryId> = store
                .list_libraries()?
                .into_iter()
                .filter(|lib| {
                    lib.kind == domain::MediaKind::Tv
                        && crate::http::library::library_visible_with_role(
                            store, &lib, user_id, is_admin,
                        )
                })
                .filter_map(|lib| lib.id.parse::<domain::LibraryId>().ok())
                .collect();
            let media_ids: Vec<domain::MediaId> = store
                .get_media_by_alias_kind("tmdb", tmdb_id, Some(domain::MediaKind::Tv))?
                .map(|m| vec![m.id])
                .unwrap_or_default();
            let panorama = store.episode_panorama(
                &media_ids,
                &visible_libs,
                visible_libs.first().copied(),
                "",
                release::parse,
            )?;
            Ok::<_, store::StoreError>((media_ids, panorama))
        },
    );
    match result {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(error)) | Err(error) => {
            tracing::warn!(%error, tmdb_id, "failed to load TV episode panorama");
            (Vec::new(), store::EpisodePanorama::empty())
        }
    }
}

pub(in crate::http) async fn enrich_library_season_summaries(
    state: &ApiState,
    panorama: Option<store::EpisodePanorama>,
    tmdb_id: Option<&str>,
    max_seasons: u32,
) -> Vec<store::SeasonAvailabilitySummary> {
    let Some(mut p) = panorama else {
        return Vec::new();
    };
    let Some(tid) = tmdb_id.map(str::to_string) else {
        return p.library_season_summaries();
    };
    let fallback = p.library_season_summaries();
    let mut candidates = p.known_seasons();
    candidates.extend(1..=max_seasons.min(50));
    let catalog = state.catalog.clone();
    state
        .http_work
        .run(crate::http::library::BROWSE_DEADLINE, move || {
            for s in candidates {
                if let Ok(Some(eps)) = catalog.cached_season_episodes(&tid, s) {
                    p.record_cached_season_episodes(s, eps.into_iter().map(|e| e.episode_number));
                }
            }
            p.library_season_summaries()
        })
        .await
        .unwrap_or(fallback)
}

pub(in crate::http) fn append_concrete_season_stubs(
    season: Option<u32>,
    panorama: &store::EpisodePanorama,
    episodes: &mut Vec<store::library_query::LibraryEpisode>,
) {
    let Some(s) = season else {
        return;
    };
    for ep_num in panorama.concrete_units_in_season(s) {
        if !episodes
            .iter()
            .any(|e| e.season == s && e.episode == ep_num)
        {
            episodes.push(store::library_query::LibraryEpisode {
                season: s,
                episode: ep_num,
                title: None,
                overview: None,
                air_date: None,
                runtime_minutes: None,
                rating: None,
                still_url: None,
                owned: false,
                file_ids: Vec::new(),
            });
        }
    }
    episodes.sort_by_key(|e| (e.season, e.episode));
}

pub(in crate::http) async fn render_library_episode_rows(
    state: &ApiState,
    mut episodes: Vec<store::library_query::LibraryEpisode>,
    season: Option<u32>,
    tmdb_id: Option<String>,
    item_id: &str,
    effective_states: &std::collections::HashMap<(u32, u32), Option<store::UnitState>>,
    panorama: &store::EpisodePanorama,
) -> Vec<Value> {
    if let (Some(s), Some(tid)) = (season, tmdb_id) {
        let catalog = state.catalog.clone();
        if let Ok((Some(cached_eps), meta_eps)) = state
            .http_work
            .run(crate::http::library::BROWSE_DEADLINE, move || {
                (
                    catalog.cached_season_episodes(&tid, s).ok().flatten(),
                    catalog
                        .cached_season_details(&tid, s)
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                )
            })
            .await
        {
            for ep in cached_eps {
                if episodes
                    .iter()
                    .any(|e| e.season == s && e.episode == ep.episode_number)
                {
                    continue;
                }
                let m = meta_eps
                    .iter()
                    .find(|m| m.episode_number == ep.episode_number);
                episodes.push(store::library_query::LibraryEpisode {
                    season: s,
                    episode: ep.episode_number,
                    title: m.and_then(|m| m.name.clone()),
                    overview: m.and_then(|m| m.overview.clone()),
                    air_date: ep.air_date,
                    runtime_minutes: m.and_then(|m| m.runtime_minutes),
                    rating: m.and_then(|m| m.rating.as_deref()?.parse::<f64>().ok()),
                    still_url: m.and_then(|m| m.still_path.clone()),
                    owned: false,
                    file_ids: Vec::new(),
                });
            }
            episodes.sort_by_key(|e| (e.season, e.episode));
        }
    }
    episodes
        .iter()
        .map(|ep| library_episode_row_json(ep, item_id, effective_states, panorama))
        .collect()
}

fn library_episode_row_json(
    ep: &store::library_query::LibraryEpisode,
    item_id: &str,
    effective_states: &std::collections::HashMap<(u32, u32), Option<store::UnitState>>,
    panorama: &store::EpisodePanorama,
) -> Value {
    let (position_ms, played, progress_percent) = match effective_states
        .get(&(ep.season, ep.episode))
        .and_then(Option::as_ref)
    {
        Some(st) => {
            let pct = (!st.played)
                .then(|| st.duration_ms.filter(|d| *d > 0))
                .flatten()
                .map(|d| ((st.position_ms * 100) / d).clamp(0, 100));
            (st.position_ms, st.played, pct)
        }
        None => (0, false, None),
    };
    let avail = panorama.episode_availability(ep.season, ep.episode, ep.air_date.as_deref());
    let (status, resolution) = if ep.owned {
        (
            store::EpisodeAvailabilityStatus::InLibrary.as_str(),
            avail.owned_in.and_then(|o| o.resolution),
        )
    } else {
        let st = match avail.status {
            store::EpisodeAvailabilityStatus::InLibrary => {
                store::EpisodeAvailabilityStatus::Missing
            }
            other => other,
        };
        (st.as_str(), None)
    };
    json!({
        "episode_number": ep.episode,
        "name": ep.title.clone().unwrap_or_else(|| format!("第 {} 集", ep.episode)),
        "overview": ep.overview,
        "air_date": ep.air_date,
        "still_url": ep.still_url.as_deref().map(episode_still_url),
        "owned": ep.owned,
        "status": status,
        "resolution": resolution,
        "file_ids": ep.file_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "position_ms": position_ms,
        "played": played,
        "progress_percent": progress_percent,
        "media_item_id": item_id,
        "season_number": ep.season,
        "runtime_minutes": ep.runtime_minutes,
        "rating": ep.rating,
    })
}
