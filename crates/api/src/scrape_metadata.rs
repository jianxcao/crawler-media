//! Shared TMDB → NFO mapping for every metadata scrape entry point.
use domain::{EntityScope, MetadataAdoption, MetadataPreferences};


use crate::catalog::Catalog;
use crate::scrape_store::ScrapeStoreExt;
use domain::{LedgerRow, Media};
use std::path::{Path, PathBuf};

pub(crate) mod targets;
/// Persist validated TMDB data through normalized source snapshots, independent of NFO output.
pub(crate) fn adopt_tmdb_metadata(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    scope: EntityScope,
    meta: &media::ItemMeta,
) {
    let preferences = state.store.lock().get_scrape_config().ok()
        .map(|config| MetadataPreferences {
            language_priority: config.effective.language_priority,
            cert_country_priority: config.effective.cert_country_priority,
        })
        .unwrap_or_else(|| MetadataPreferences {
            language_priority: crate::scrape_config::default_language_priority(),
            cert_country_priority: crate::scrape_config::default_cert_country_priority(),
        });
    let mut payload = serde_json::Map::new();
    if let Some(value) = &meta.overview { payload.insert("overview".into(), value.clone().into()); }
    if let Some(value) = &meta.tagline { payload.insert("tagline".into(), value.clone().into()); }
    if let Some(value) = &meta.rating { if let Ok(value) = value.parse::<f64>() { payload.insert("vote_average".into(), value.into()); } }
    if let Some(value) = meta.vote_count { payload.insert("vote_count".into(), value.into()); }
    if let Some(value) = meta.runtime_minutes.as_deref().and_then(|value| value.parse::<u32>().ok()) { payload.insert("runtime".into(), value.into()); }
    payload.insert("title".into(), media.title.clone().into());
    if let Some(value) = &media.original_title { payload.insert("original_title".into(), value.clone().into()); }
    let release_key = if media.kind == domain::MediaKind::Tv { "first_air_date" } else { "release_date" };
    if let Some(value) = &meta.release_date { payload.insert(release_key.into(), value.clone().into()); }
    if let Some(value) = &meta.last_air_date { payload.insert("last_air_date".into(), value.clone().into()); }
    if let Some(value) = &meta.original_language { payload.insert("original_language".into(), value.clone().into()); }
    if let Some(value) = &meta.status { payload.insert("status".into(), value.clone().into()); }
    if !meta.studios.is_empty() { payload.insert("production_companies".into(), meta.studios.iter().map(|name| serde_json::json!({"name":name})).collect::<Vec<_>>().into()); }
    if !meta.genres.is_empty() { payload.insert("genres".into(), meta.genres.iter().enumerate().map(|(index,name)| serde_json::json!({"id":meta.genre_ids.get(index),"name":name})).collect::<Vec<_>>().into()); }
    if !meta.origin_countries.is_empty() { payload.insert("origin_country".into(), serde_json::json!(meta.origin_countries)); }
    let normalized = match media::normalize_metadata("tmdb", scope, &serde_json::Value::Object(payload).to_string(), &preferences) {
        Ok(normalized) => normalized,
        Err(error) => { tracing::warn!(%error, media_id=%media.id, "TMDB metadata normalization failed"); return; }
    };
    let locale = preferred_language(state);
    let fetched_at = crate::job_loop::unix_now() * 1000;
    let adoption = MetadataAdoption {
        media_id: media.id, scope, locale, source: "tmdb".into(),
        provider_entity_id: tmdb_id.to_string(), provider_work_id: tmdb_id.to_string(),
        fetched_at, normalized, expected_revision: None, preferences,
    };
    if let Err(error) = state.store.lock().adopt_metadata(&adoption, fetched_at) {
        tracing::warn!(%error, media_id=%media.id, "TMDB metadata adoption failed");
    }
}

fn adopt_tmdb_episode(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    season: u32,
    episode_number: u32,
    episode: &media::EpisodeMeta,
    locale: &str,
) {
    let preferences = MetadataPreferences { language_priority: vec![locale.to_string()], cert_country_priority: Vec::new() };
    let mut payload = serde_json::Map::new();
    if let Some(value) = &episode.name { payload.insert("title".into(), value.clone().into()); }
    if let Some(value) = &episode.overview { payload.insert("overview".into(), value.clone().into()); }
    if let Some(value) = &episode.air_date { payload.insert("air_date".into(), value.clone().into()); }
    if let Some(value) = episode.runtime_minutes { payload.insert("runtime".into(), value.into()); }
    if let Some(value) = episode.rating.as_deref().and_then(|value| value.parse::<f64>().ok()) { payload.insert("vote_average".into(), value.into()); }
    if let Some(value) = episode.vote_count { payload.insert("vote_count".into(), value.into()); }
    if let Some(value) = &episode.still_path { payload.insert("still_path".into(), value.clone().into()); }
    let scope = EntityScope::Episode { season, episode: episode_number };
    let normalized = match media::normalize_metadata("tmdb", scope, &serde_json::Value::Object(payload).to_string(), &preferences) {
        Ok(value) => value,
        Err(error) => { tracing::warn!(%error, media_id=%media.id, season, episode_number, "episode metadata normalization failed"); return; }
    };
    let fetched_at = crate::job_loop::unix_now() * 1000;
    let adoption = MetadataAdoption {
        media_id: media.id, scope, locale: locale.to_string(), source: "tmdb".into(),
        provider_entity_id: format!("{tmdb_id}:{season}:{episode_number}"), provider_work_id: tmdb_id.to_string(),
        fetched_at, normalized, expected_revision: None, preferences,
    };
    if let Err(error) = state.store.lock().adopt_metadata(&adoption, fetched_at) {
        tracing::warn!(%error, media_id=%media.id, season, episode_number, "episode metadata adoption failed");
    }
}


fn adopt_tmdb_season(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    season: &media::TvSeason,
    locale: &str,
) {
    let preferences = MetadataPreferences { language_priority: vec![locale.to_string()], cert_country_priority: Vec::new() };
    let mut payload = serde_json::Map::new();
    payload.insert("name".into(), season.name.clone().into());
    if let Some(value) = &season.overview { payload.insert("overview".into(), value.clone().into()); }
    if let Some(value) = &season.air_date { payload.insert("air_date".into(), value.clone().into()); }
    if let Some(value) = season.episode_count { payload.insert("episode_count".into(), value.into()); }
    if let Some(value) = &season.poster_path { payload.insert("poster_path".into(), value.clone().into()); }
    let scope = EntityScope::Season(season.season_number);
    let normalized = match media::normalize_metadata("tmdb", scope, &serde_json::Value::Object(payload).to_string(), &preferences) {
        Ok(value) => value,
        Err(error) => { tracing::warn!(%error, media_id=%media.id, season=season.season_number, "season metadata normalization failed"); return; }
    };
    let fetched_at = crate::job_loop::unix_now() * 1000;
    let adoption = MetadataAdoption {
        media_id: media.id, scope, locale: locale.to_string(), source: "tmdb".into(),
        provider_entity_id: format!("{tmdb_id}:{}", season.season_number), provider_work_id: tmdb_id.to_string(),
        fetched_at, normalized, expected_revision: None, preferences,
    };
    if let Err(error) = state.store.lock().adopt_metadata(&adoption, fetched_at) {
        tracing::warn!(%error, media_id=%media.id, season=season.season_number, "season metadata adoption failed");
    }
}
struct LeasedArtworkFetch<'a> {
    fetch: &'a dyn crate::poster_fetch::PosterFetch,
    lease: Option<&'a crate::library_lifecycle::LibraryWorkLease>,
}
impl crate::poster_fetch::PosterFetch for LeasedArtworkFetch<'_> {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        if let Some(lease) = self.lease { lease.control().check().map_err(|e| e.to_string())?; }
        let bytes = self.fetch.get(url)?;
        if let Some(lease) = self.lease { lease.control().check().map_err(|e| e.to_string())?; }
        Ok(bytes)
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        match self.lease {
            Some(lease) => lease.publish(|| self.fetch.write(path, bytes).map_err(|error| error.to_string()))
                .map_err(std::io::Error::other),
            None => self.fetch.write(path, bytes),
        }
    }
}

pub(crate) fn fetch_tmdb_metadata(
    state: &crate::management::ApiState,
    kind: domain::MediaKind,
    tmdb_id: &str,
) -> Result<Option<media::ItemMeta>, String> {
    let preferences = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| {
            (
                config.effective.language_priority,
                config.effective.cert_country_priority,
            )
        })
        .unwrap_or_else(|| {
            (
                crate::scrape_config::default_language_priority(),
                crate::scrape_config::default_cert_country_priority(),
            )
        });
    state
        .catalog
        .metadata_with_preferences(kind, tmdb_id, &preferences.0, &preferences.1)
}

pub(crate) fn preferred_language(state: &crate::management::ApiState) -> String {
    state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.primary_language().to_string())
        .unwrap_or_else(|| "zh-CN".into())
}

/// NFO sidecars to read for one ledger row, most specific first. TV keeps the
/// show-level `tvshow.nfo`; movies may carry either `<file>.nfo` or `movie.nfo`.
/// Shared so browse and wall never disagree about where metadata lives.
pub(crate) fn nfo_candidates(
    path: &Path,
    row: &LedgerRow,
    kind: domain::MediaKind,
) -> Vec<PathBuf> {
    match kind {
        domain::MediaKind::Tv => vec![show_root(path, row).join("tvshow.nfo")],
        _ => {
            let directory = path.parent().unwrap_or(path);
            vec![path.with_extension("nfo"), directory.join("movie.nfo")]
        }
    }
}

pub(crate) fn show_root(path: &Path, row: &LedgerRow) -> PathBuf {
    let directory = path.parent().unwrap_or(path);
    let dir_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let is_season_directory = dir_name.starts_with("season")
        || (dir_name.contains(".s0") || dir_name.contains(".s1") || dir_name.contains(".s2"));
    if row.season.is_some() && is_season_directory {
        directory.parent().unwrap_or(directory).to_path_buf()
    } else {
        directory.to_path_buf()
    }
}

pub(crate) fn write_nfo(path: &Path, media: &Media, metadata: &library::NfoMeta) -> bool {
    match library::write_nfo(path, media, Some(metadata)) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to write scraped NFO metadata");
            false
        }
    }
}

pub(crate) fn write_series_nfos(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    rows: &[LedgerRow],
    show_metadata: &library::NfoMeta,
    language: &str,
) -> Vec<(PathBuf, Option<String>)> {
    let target = show_root.join("tvshow.nfo");
    let lease = match state.acquire_path_work(&target, media.kind) {
        Ok(Some(lease)) => lease, _ => return Vec::new(),
    };
    let rows: Vec<_> = {
        let store = state.store.lock();
        rows.iter().filter(|row| store.library_for_path_strict(Path::new(&row.path), media.kind)
            .ok().flatten().is_some_and(|owner| owner.id == lease.library_id())
            && self::show_root(Path::new(&row.path), row) == show_root).cloned().collect()
    };
    let catalog = state.catalog.as_ref();
    if lease.publish(|| Ok(write_nfo(&target, media, show_metadata))).is_err() { return Vec::new(); }
    let mut matched_episodes = Vec::new();
    let seasons = rows
        .iter()
        .filter_map(|row| row.season)
        .collect::<std::collections::BTreeSet<_>>();
    for season in seasons {
        let episodes = match catalog.season_details_lang(tmdb_id, season, language) {
            Ok(episodes) if !episodes.is_empty() => episodes,
            Ok(_) => match catalog.season_details(tmdb_id, season) {
                Ok(episodes) => episodes,
                Err(error) => {
                    tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                    continue;
                }
            },
            Err(error) => {
                tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                continue;
            }
        };
        for row in rows.iter().filter(|row| row.season == Some(season)) {
            if lease.control().is_cancelled() { return matched_episodes; }
            let Some(episode_number) = row.episode else {
                continue;
            };
            if !targets::video_present(Path::new(&row.path)) {
                continue;
            }
            let Some(episode) = episodes
                .iter()
                .find(|episode| episode.episode_number == episode_number)
                .cloned()
            else {
                continue;
            };
            let path = Path::new(&row.path).with_extension("nfo");
            let mut still_path = episode.still_path.clone();
            if still_path.is_none() {
                if let Ok(stills) = catalog.episode_stills(tmdb_id, season, episode_number) {
                    still_path = stills.into_iter().next();
                }
            }
            adopt_tmdb_episode(state, media, tmdb_id, season, episode_number, &episode, language);
            if lease.publish(|| Ok(write_nfo(&path, media, &nfo_from_episode(row, episode)))).is_err() { return matched_episodes; }
            matched_episodes.push((PathBuf::from(&row.path), still_path));
        }
    }
    matched_episodes
}

pub(crate) fn nfo_from_tmdb(media: &Media, metadata: &media::ItemMeta) -> library::NfoMeta {
    library::NfoMeta {
        original_title: media.original_title.clone(),
        plot: metadata.overview.clone(),
        rating: metadata.rating.clone(),
        runtime_minutes: metadata.runtime_minutes.clone(),
        tagline: metadata.tagline.clone(),
        premiered: metadata.release_date.clone(),
        end_date: metadata.last_air_date.clone(),
        content_rating: metadata.content_rating.clone(),
        vote_count: metadata.vote_count,
        original_language: metadata.original_language.clone(),
        countries: metadata.origin_countries.clone(),
        studios: metadata.studios.clone(),
        studio_logo: metadata.studio_logo.clone(),
        number_of_seasons: metadata.number_of_seasons,
        number_of_episodes: metadata.number_of_episodes,
        directors: metadata.directors.clone(),
        creators: metadata.creators.clone(),
        genres: metadata.genres.clone(),
        cast: metadata
            .cast
            .iter()
            .map(|member| library::CastMember {
                name: member.name.clone(),
                role: member.role.clone(),
                tmdb_id: member.person_id.map(|id| id.to_string()),
                thumb: member.avatar_path.clone(),
                order: Some(member.order),
            })
            .collect(),
        ..Default::default()
    }
}

pub(crate) fn nfo_from_episode(row: &LedgerRow, episode: media::EpisodeMeta) -> library::NfoMeta {
    library::NfoMeta {
        title: episode.name,
        plot: episode.overview,
        runtime_minutes: episode.runtime_minutes.map(|value| value.to_string()),
        rating: episode.rating,
        vote_count: episode.vote_count,
        aired: episode.air_date,
        season: row.season.map(|value| value as u32),
        episode: row.episode,
        thumb: episode
            .still_path
            .map(|path| format!("https://image.tmdb.org/t/p/w300{path}")),
        ..Default::default()
    }
}

struct PosterFetchCatalogGet<'a> {
    fetch: &'a dyn crate::poster_fetch::PosterFetch,
}

impl media::CatalogGet for PosterFetchCatalogGet<'_> {
    fn get(&self, path: &str) -> Result<String, media::TmdbError> {
        let bytes = self
            .fetch
            .get(path)
            .map_err(media::TmdbError::Http)?;
        String::from_utf8(bytes).map_err(|e| media::TmdbError::Parse(e.to_string()))
    }
}

pub(crate) fn scrape_tv_sidecars(
    state: &crate::management::ApiState,
    media: &Media,
    _show_root: &Path,
    rows: &[LedgerRow],
) {
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let path_leases: Vec<_> = rows.iter().filter_map(|row| {
        state.acquire_path_work(Path::new(&row.path), media.kind).ok()
            .map(|lease| (row.clone(), lease))
    }).collect();
    let allowed_rows: Vec<_> = path_leases.iter().map(|(row, _)| row.clone()).collect();
    let groups = targets::live_groups(&allowed_rows);
    if groups.is_empty() {
        tracing::debug!(media = %media.title, "没有可用的视频文件，跳过电视剧刮削");
        return;
    }
    tracing::info!(media = %media.title, "开始电视剧刮削与附属文件处理");
    let scrape_config = state.store.lock().get_scrape_config().ok();
    let (mirror_nfo, mirror_thumbs, mirror_images, still_size, fanart_api_key, fanart_lang) =
        match scrape_config {
            Some(ref c) => (
                c.effective.mirror_nfo,
                c.effective.mirror_episode_thumbs,
                c.effective.mirror_images,
                c.effective.still_size.clone(),
                c.effective.fanart_api_key.clone(),
                c.effective.fanart_language.clone(),
            ),
            None => (
                true,
                true,
                true,
                "w300".to_string(),
                Some(crate::scrape_config::DEFAULT_FANART_API_KEY.to_string()),
                vec!["zh".into(), "en".into()],
            ),
        };

    for (effective_root, group_rows) in groups {
        let matched_episodes = scrape_tv_episodes_and_nfos(
            state,
            media,
            tmdb_id,
            &effective_root,
            &group_rows,
            mirror_nfo,
        );

        if mirror_thumbs {
            scrape_tv_stills(state, matched_episodes, &still_size);
        }

        if mirror_images {
            if let Some(ref api_key) = fanart_api_key {
                scrape_tv_fanart(state, media, &effective_root, &group_rows, api_key, &fanart_lang);
            }
        }
    }
    tracing::info!(media = %media.title, "完成电视剧刮削与附属文件处理");
}

fn scrape_tv_episodes_and_nfos(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    rows: &[LedgerRow],
    mirror_nfo: bool,
) -> Vec<(PathBuf, Option<String>)> {
    let tmdb_meta = fetch_tmdb_metadata(state, media.kind, tmdb_id).ok().flatten();
    if let Some(meta) = &tmdb_meta {
        adopt_tmdb_metadata(state, media, tmdb_id, EntityScope::Work, meta);
        record_scraped_display(state, media.id, meta);
    }
    let show_nfo = tmdb_meta.as_ref().map(|meta| nfo_from_tmdb(media, meta)).unwrap_or_default();
    let preferred_lang = preferred_language(state);

    let tv_seasons = match state.catalog.tv_seasons(tmdb_id) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(tmdb_id, error = %err, "获取 TV 季列表失败");
            Vec::new()
        }
    };

    let owned_seasons: std::collections::HashSet<u32> = rows.iter().filter_map(|r| r.season).collect();
    for season_meta in &tv_seasons {
        if owned_seasons.contains(&season_meta.season_number) {
            adopt_tmdb_season(state, media, tmdb_id, season_meta, &preferred_lang);
        }
    }
    let season_artworks: Vec<crate::fanart_artwork::SeasonArtwork> = tv_seasons
        .into_iter()
        .filter(|s| owned_seasons.contains(&(s.season_number as u32)))
        .map(|s| crate::fanart_artwork::SeasonArtwork {
            number: s.season_number as u32,
            name: Some(s.name),
            overview: s.overview,
            air_date: s.air_date,
            poster_url: s
                .poster_path
                .as_deref()
                .filter(|p| p.starts_with('/'))
                .map(|p| format!("https://image.tmdb.org/t/p/w780{p}")),
        })
        .collect();

    if mirror_nfo {
        let mut ep_matches = write_series_nfos(
            state,
            media,
            tmdb_id,
            show_root,
            rows,
            &show_nfo,
            &preferred_lang,
        );
        if let Ok(lease) = state.acquire_path_work(show_root, media.kind) {
            let write = || { crate::fanart_artwork::write_season_nfos(show_root, rows, &season_artworks); Ok(()) };
            let _ = match lease { Some(lease) => lease.publish(write), None => write() };
        }
        for row in rows {
            let row_path = PathBuf::from(&row.path);
            if !ep_matches.iter().any(|(p, _)| p == &row_path) {
                let still_path = row.season.zip(row.episode).and_then(|(s, e)| {
                    state
                        .catalog
                        .episode_stills(tmdb_id, s, e)
                        .ok()
                        .and_then(|mut v| v.drain(..).next())
                });
                ep_matches.push((row_path, still_path));
            }
        }
        ep_matches
    } else {
        let mut ep_matches = Vec::new();
        for season in &owned_seasons {
            let episodes = state.catalog.season_details(tmdb_id, *season).unwrap_or_default();
            for row in rows.iter().filter(|r| r.season == Some(*season)) {
                if let Some(ep_num) = row.episode {
                    let mut still_path = episodes
                        .iter()
                        .find(|e| e.episode_number == ep_num)
                        .and_then(|ep| ep.still_path.clone());
                    if still_path.is_none() {
                        if let Ok(stills) = state.catalog.episode_stills(tmdb_id, *season, ep_num) {
                            still_path = stills.into_iter().next();
                        }
                    }
                    ep_matches.push((PathBuf::from(&row.path), still_path));
                }
            }
        }
        ep_matches
    }
}

fn scrape_tv_stills(
    state: &crate::management::ApiState,
    matched_episodes: Vec<(PathBuf, Option<String>)>,
    still_size: &str,
) {
    scrape_tv_stills_with(state, matched_episodes, still_size, |video, at, target| {
        let lease = state.acquire_path_work(video, domain::MediaKind::Tv)?;
        let control = lease.as_ref().map(|lease| lease.control().clone()).unwrap_or_default();
        library::extract_frame_controlled(video, at, target, &control).map_err(|error| error.to_string())
    });
}

fn scrape_tv_stills_with(
    state: &crate::management::ApiState,
    matched_episodes: Vec<(PathBuf, Option<String>)>,
    still_size: &str,
    extract: impl Fn(&Path, i64, &Path) -> Result<(), String>,
) {
    let is_allowed = |path: &Path| -> bool {
        let store = state.store.lock();
        if let Ok(libraries) = store.list_libraries() {
            libraries
                .into_iter()
                .filter(|lib| lib.root_paths.iter().any(|r| path.starts_with(r)))
                .max_by_key(|lib| {
                    lib.root_paths
                        .iter()
                        .filter(|r| path.starts_with(r))
                        .map(|r| r.components().count())
                        .max()
                        .unwrap_or(0)
                })
                .map(|lib| lib.generate_thumbnails)
                .unwrap_or(true)
        } else {
            true
        }
    };

    for (ep_path, still_path) in matched_episodes {
        if !targets::video_present(&ep_path) {
            continue;
        }
        let Ok(lease) = state.acquire_path_work(&ep_path, domain::MediaKind::Tv) else { continue };
        let fetch = LeasedArtworkFetch { fetch: state.poster_fetch.as_ref(), lease: lease.as_ref() };
        let ok = crate::fanart_artwork::mirror_episode_still(
            &fetch,
            &ep_path,
            still_path.as_deref(),
            still_size,
        );
        if lease.as_ref().is_some_and(|lease| lease.control().is_cancelled()) { continue; }
        if !ok && targets::video_present(&ep_path) && is_allowed(&ep_path) {
            if crate::episode_still::existing(&ep_path).is_none() {
                let still_dest = crate::episode_still::path(&ep_path);
                if let Err(err) = extract(&ep_path, 60_000, &still_dest) {
                    tracing::warn!(error = %err, path = %ep_path.display(), "ffmpeg 抽取剧照失败");
                }
            }
        }
        crate::poster_fetch::record_discovered_artwork(state, &ep_path);
    }
}

#[cfg(test)]
mod still_fallback_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct EmptyFetcher;
    impl indexer::Fetcher for EmptyFetcher {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unused".into()))
        }
    }

    struct RenameFetch {
        video: PathBuf,
    }
    impl crate::poster_fetch::PosterFetch for RenameFetch {
        fn get(&self, _: &str) -> Result<Vec<u8>, String> {
            let _ = std::fs::remove_file(&self.video);
            Err("download failed after rename".into())
        }
    }

    #[test]
    fn still_fallback_skips_ffmpeg_when_source_vanishes_during_download() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("S01E01.mkv");
        std::fs::write(&video, b"video").unwrap();
        let called = AtomicBool::new(false);
        let state = crate::management::ApiState::new(
            crate::Store::open(tmp.path().join("data")).unwrap(),
            "test-token".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap()
        .with_poster_fetch(Arc::new(RenameFetch {
            video: video.clone(),
        }));
        scrape_tv_stills_with(
            &state,
            vec![(video.clone(), Some("/ep1.jpg".into()))],
            "w300",
            |_, _, _| {
                called.store(true, Ordering::SeqCst);
                panic!("ffmpeg must not run after the source disappears");
            },
        );
        assert!(!called.load(Ordering::SeqCst));
        assert!(!video.exists());
    }
}

fn scrape_tv_fanart(
    state: &crate::management::ApiState,
    media: &Media,
    show_root: &Path,
    rows: &[LedgerRow],
    api_key: &str,
    fanart_lang: &[String],
) {
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let tvdb_id = match media.tvdb_id.clone() {
        Some(id) if !id.is_empty() => Some(id),
        _ => match state.catalog.tvdb_id(tmdb_id) {
            Ok(Some(id)) if !id.is_empty() => {
                let mut updated = media.clone();
                updated.tvdb_id = Some(id.clone());
                let _ = state.store.lock().update_media(&updated);
                Some(id)
            }
            _ => None,
        },
    };

    let Some(tvdb_id) = tvdb_id else {
        tracing::info!(media = %media.title, "缺少 tvdb_id，跳过 Fanart.tv 抓取");
        return;
    };

    let Ok(lease) = state.acquire_path_work(show_root, media.kind) else { return };
    let fetch = LeasedArtworkFetch { fetch: state.poster_fetch.as_ref(), lease: lease.as_ref() };
    let getter = PosterFetchCatalogGet { fetch: &fetch };
    let fanart_db = state.store.lock().cache_db_path();
    match media::fanart::FanartClient::new(getter, &fanart_db, api_key) {
        Ok(client) => {
            let lang_refs: Vec<&str> = fanart_lang.iter().map(|s| s.as_str()).collect();
            match client.tv(&tvdb_id, &lang_refs) {
                Ok(set) => {
                    crate::fanart_artwork::save_fanart_files(
                        &fetch,
                        show_root,
                        rows,
                        &set,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, %tvdb_id, "请求 Fanart.tv 失败");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "初始化 FanartClient 失败");
        }
    }
}

pub(crate) fn scrape_movie_fanart(
    state: &crate::management::ApiState,
    media: &Media,
    movie_root: &Path,
) {
    if !movie_root.is_dir() {
        return;
    }
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let scrape_config = state.store.lock().get_scrape_config().ok();
    let (mirror_images, fanart_api_key, fanart_lang) = match scrape_config {
        Some(ref c) => (
            c.effective.mirror_images,
            c.effective.fanart_api_key.clone(),
            c.effective.fanart_language.clone(),
        ),
        None => (
            true,
            Some(crate::scrape_config::DEFAULT_FANART_API_KEY.to_string()),
            vec!["zh".into(), "en".into()],
        ),
    };
    if !mirror_images {
        return;
    }
    let Some(api_key) = fanart_api_key else {
        return;
    };

    let Ok(lease) = state.acquire_path_work(movie_root, media.kind) else { return };
    let fetch = LeasedArtworkFetch { fetch: state.poster_fetch.as_ref(), lease: lease.as_ref() };
    let getter = PosterFetchCatalogGet { fetch: &fetch };
    let fanart_db = state.store.lock().cache_db_path();
    match media::fanart::FanartClient::new(getter, &fanart_db, &api_key) {
        Ok(client) => {
            let lang_refs: Vec<&str> = fanart_lang.iter().map(|s| s.as_str()).collect();
            match client.movie(tmdb_id, &lang_refs) {
                Ok(set) => {
                    crate::fanart_artwork::save_fanart_files(
                        &fetch,
                        movie_root,
                        &[],
                        &set,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, %tmdb_id, "请求电影 Fanart.tv 失败");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "初始化 FanartClient 失败");
        }
    }
}

/// Persist the display facts a Scrape observed (rating, release date, studio, logo).
///
/// They are written with the adopted metadata and the affected cards are reduced in the
/// same transaction, so the poster wall and the detail view read one source of truth.
pub(crate) fn record_scraped_display(
    state: &crate::management::ApiState,
    media_id: domain::MediaId,
    meta: &media::ItemMeta,
) {
    let rating = meta
        .vote_count
        .filter(|&count| count >= 5)
        .and_then(|_| meta.rating.as_ref().and_then(|raw| raw.parse::<f64>().ok()));
    let locale = metadata_locale(state);
    let display = ::store::metadata::ScrapedDisplay {
        rating,
        release_date: meta.release_date.as_deref(),
        studio: meta.studios.first().map(String::as_str),
        studio_logo: meta.studio_logo.as_deref(),
    };
    if let Err(error) = state
        .store
        .lock()
        .record_scraped_display(media_id, &locale, display)
    {
        tracing::error!(%error, %media_id, "记录刮削展示字段失败");
    }
}

/// Locale the instance displays metadata in (`settings.metadata.language`).
pub(crate) fn metadata_locale(state: &crate::management::ApiState) -> String {
    state
        .store
        .lock()
        .get_setting(crate::settings_keys::METADATA_LANGUAGE)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "zh-CN".to_string())
}

/// Same as [`record_scraped_display`] but reading the values the NFO writer uses.
///
/// The scan path may have rewritten the display facts through the NFO-shaped struct
/// (`premiered`/`studios`/`rating` from the NFO meta), so the card must use those exact
/// values instead of re-deriving them.
pub(crate) fn record_scraped_display_meta(
    state: &crate::management::ApiState,
    media_id: domain::MediaId,
    meta: &media::ItemMeta,
    nfo: &library::NfoMeta,
) {
    let rating = meta
        .vote_count
        .filter(|&count| count >= 5)
        .and_then(|_| nfo.rating.as_ref().and_then(|raw| raw.parse::<f64>().ok()));
    let locale = metadata_locale(state);
    let display = ::store::metadata::ScrapedDisplay {
        rating,
        release_date: nfo.premiered.as_deref(),
        studio: nfo.studios.first().map(String::as_str),
        studio_logo: nfo.studio_logo.as_deref(),
    };
    if let Err(error) = state
        .store
        .lock()
        .record_scraped_display(media_id, &locale, display)
    {
        tracing::error!(%error, %media_id, "记录刮削展示字段失败");
    }
}
