use domain::{EntityScope, MetadataAdoption, MetadataPreferences};

use crate::scrape_store::ScrapeStoreExt;
use domain::Media;
use std::path::{Path, PathBuf};

use super::targets;
use super::work::{
    LeasedArtworkFetch, adopt_tmdb_metadata, fetch_tmdb_metadata, nfo_from_tmdb,
    preferred_language, record_scraped_display, write_nfo,
};

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

pub(crate) fn write_series_nfos(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    coverages: &[domain::FileCoverage],
    show_metadata: &library::NfoMeta,
    language: &str,
) -> Vec<(PathBuf, Option<String>)> {
    let target = show_root.join("tvshow.nfo");
    let lease = match state.acquire_path_work(&target, media.kind) {
        Ok(Some(lease)) => lease, _ => return Vec::new(),
    };
    let rows: Vec<_> = {
        let store = state.store.lock();
        coverages.iter().filter(|coverage| store.library_for_path_strict(&coverage.file.path, media.kind)
            .ok().flatten().is_some_and(|owner| owner.id == lease.library_id())
            && super::show_root(&coverage.file.path, coverage) == show_root).cloned().collect()
    };
    let catalog = state.catalog.as_ref();
    if lease.publish(|| Ok(write_nfo(&target, media, show_metadata))).is_err() { return Vec::new(); }
    let mut matched_episodes = Vec::new();
    let seasons = rows
        .iter()
        .flat_map(|coverage| coverage.units.iter())
        .filter_map(|unit| unit.episode_coordinates())
        .map(|(season, _)| season)
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
        for row in &rows {
            for unit in &row.units {
                let Some((unit_season, episode_number)) = unit.episode_coordinates() else {
                    continue;
                };
                if unit_season != season {
                    continue;
                }
                if lease.control().is_cancelled() { return matched_episodes; }
                if !targets::video_present(&row.file.path) {
                    continue;
                }
                let Some(episode) = episodes
                    .iter()
                    .find(|episode| episode.episode_number == episode_number)
                    .cloned()
                else {
                    continue;
                };
                let path = row.file.path.with_extension("nfo");
                let mut still_path = episode.still_path.clone();
                if still_path.is_none() {
                    if let Ok(stills) = catalog.episode_stills(tmdb_id, season, episode_number) {
                        still_path = stills.into_iter().next();
                    }
                }
                adopt_tmdb_episode(state, media, tmdb_id, season, episode_number, &episode, language);
                let episode_coverage = domain::FileCoverage {
                    file: row.file.clone(),
                    units: vec![*unit],
                };
                if lease.publish(|| Ok(write_nfo(&path, media, &nfo_from_episode(&episode_coverage, episode)))).is_err() { return matched_episodes; }
                matched_episodes.push((row.file.path.clone(), still_path));
            }
        }
    }
    matched_episodes
}

pub(crate) fn nfo_from_episode(
    coverage: &domain::FileCoverage,
    episode: media::EpisodeMeta,
) -> library::NfoMeta {
    let coordinates = coverage
        .units
        .iter()
        .find_map(|unit| unit.episode_coordinates());
    library::NfoMeta {
        title: episode.name,
        plot: episode.overview,
        runtime_minutes: episode.runtime_minutes.map(|value| value.to_string()),
        rating: episode.rating,
        vote_count: episode.vote_count,
        aired: episode.air_date,
        season: coordinates.map(|(season, _)| season),
        episode: coordinates.map(|(_, episode)| episode),
        thumb: episode
            .still_path
            .map(|path| format!("https://image.tmdb.org/t/p/w300{path}")),
        ..Default::default()
    }
}

pub(super) struct PosterFetchCatalogGet<'a> {
    pub(super) fetch: &'a dyn crate::poster_fetch::PosterFetch,
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
    coverages: &[domain::FileCoverage],
) {
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let path_leases: Vec<_> = coverages.iter().filter_map(|coverage| {
        state.acquire_path_work(&coverage.file.path, media.kind).ok()
            .map(|lease| (coverage.clone(), lease))
    }).collect();
    let allowed_rows: Vec<_> = path_leases.iter().map(|(coverage, _)| coverage.clone()).collect();
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
    rows: &[domain::FileCoverage],
    mirror_nfo: bool,
) -> Vec<(PathBuf, Option<String>)> {
    let tmdb_meta = fetch_tmdb_metadata(state, media.kind, tmdb_id).ok().flatten();
    if let Some(meta) = &tmdb_meta {
        adopt_tmdb_metadata(state, media, tmdb_id, EntityScope::Work);
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

    let owned_seasons: std::collections::HashSet<u32> = rows
        .iter()
        .flat_map(|row| row.units.iter())
        .filter_map(|unit| unit.episode_coordinates())
        .map(|(season, _)| season)
        .collect();
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
            let row_path = row.file.path.clone();
            if !ep_matches.iter().any(|(p, _)| p == &row_path) {
                let still_path = row
                    .units
                    .iter()
                    .find_map(|unit| unit.episode_coordinates())
                    .and_then(|(s, e)| {
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
            for row in rows.iter() {
                for unit in &row.units {
                    let Some((unit_season, ep_num)) = unit.episode_coordinates() else {
                        continue;
                    };
                    if unit_season != *season {
                        continue;
                    }
                    let mut still_path = episodes
                        .iter()
                        .find(|e| e.episode_number == ep_num)
                        .and_then(|ep| ep.still_path.clone());
                    if still_path.is_none() {
                        if let Ok(stills) = state.catalog.episode_stills(tmdb_id, *season, ep_num) {
                            still_path = stills.into_iter().next();
                        }
                    }
                    ep_matches.push((row.file.path.clone(), still_path));
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
    rows: &[domain::FileCoverage],
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
                let alias = domain::MediaAlias {
                    source: "tvdb".into(),
                    kind: media.kind,
                    external_id: id.clone(),
                };
                let _ = state.store.lock().add_media_alias(media.id, &alias);
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
