use domain::EntityScope;

use crate::scrape_store::ScrapeStoreExt;
use domain::Media;
use std::path::{Path, PathBuf};

use super::series::PosterFetchCatalogGet;

/// Persist the validated full TMDB response as a normalized source snapshot,
/// independent of NFO output. See [`crate::metadata_service`].
pub(crate) fn adopt_tmdb_metadata(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    scope: EntityScope,
) {
    if let Err(error) =
        crate::metadata_service::adopt_catalog_metadata(state, media, tmdb_id, scope)
    {
        tracing::warn!(%error, media_id = %media.id, "TMDB metadata adoption failed");
    }
}

pub(super) struct LeasedArtworkFetch<'a> {
    pub(super) fetch: &'a dyn crate::poster_fetch::PosterFetch,
    pub(super) lease: Option<&'a crate::library_lifecycle::LibraryWorkLease>,
}
impl crate::poster_fetch::PosterFetch for LeasedArtworkFetch<'_> {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        if let Some(lease) = self.lease {
            lease.control().check().map_err(|e| e.to_string())?;
        }
        let bytes = self.fetch.get(url)?;
        if let Some(lease) = self.lease {
            lease.control().check().map_err(|e| e.to_string())?;
        }
        Ok(bytes)
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        match self.lease {
            Some(lease) => lease
                .publish(|| {
                    self.fetch
                        .write(path, bytes)
                        .map_err(|error| error.to_string())
                })
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

/// NFO sidecars to read for one library file, most specific first. TV keeps the
/// show-level `tvshow.nfo`; movies may carry either `<file>.nfo` or `movie.nfo`.
/// Shared so browse and wall never disagree about where metadata lives.
pub(crate) fn nfo_candidates(
    path: &Path,
    coverage: &domain::FileCoverage,
    kind: domain::MediaKind,
) -> Vec<PathBuf> {
    match kind {
        domain::MediaKind::Tv => vec![show_root(path, coverage).join("tvshow.nfo")],
        _ => {
            let directory = path.parent().unwrap_or(path);
            vec![path.with_extension("nfo"), directory.join("movie.nfo")]
        }
    }
}

pub(crate) fn show_root(path: &Path, coverage: &domain::FileCoverage) -> PathBuf {
    let directory = path.parent().unwrap_or(path);
    let dir_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let is_season_directory = dir_name.starts_with("season")
        || (dir_name.contains(".s0") || dir_name.contains(".s1") || dir_name.contains(".s2"));
    if coverage.is_episodic() && is_season_directory {
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

    let Ok(lease) = state.acquire_path_work(movie_root, media.kind) else {
        return;
    };
    let fetch = LeasedArtworkFetch {
        fetch: state.poster_fetch.as_ref(),
        lease: lease.as_ref(),
    };
    let getter = PosterFetchCatalogGet { fetch: &fetch };
    let fanart_db = state.store.lock().cache_db_path();
    match media::fanart::FanartClient::new(getter, &fanart_db, &api_key) {
        Ok(client) => {
            let lang_refs: Vec<&str> = fanart_lang.iter().map(|s| s.as_str()).collect();
            match client.movie(tmdb_id, &lang_refs) {
                Ok(set) => {
                    crate::fanart_artwork::save_fanart_files(&fetch, movie_root, &[], &set);
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
