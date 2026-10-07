use std::path::Path;

use domain::{Media, MediaKind};
use serde::Deserialize;

use crate::cache::CatalogCache;
use crate::parse::{self, CatalogHit};

#[derive(Deserialize)]
struct GenreList {
    genres: Vec<Genre>,
}

#[derive(Deserialize)]
struct Genre {
    id: i64,
    name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TmdbError {
    #[error("http: {0}")]
    Http(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub trait CatalogGet {
    fn get(&self, path: &str) -> Result<String, TmdbError>;
}

pub struct Tmdb<H> {
    http: H,
    cache: CatalogCache,
    /// Extra query params appended to every request and cache key, so a
    /// language change does not serve stale keys from another locale.
    language: String,
}

impl<H: CatalogGet> Tmdb<H> {
    pub fn new(http: H, catalog_db: &Path) -> Result<Self, TmdbError> {
        // now = 0 → CatalogCache 走实时时钟（生产路径缓存正常过期）。
        Self::new_at(http, catalog_db, 0)
    }

    pub fn with_language(mut self, language: &str) -> Self {
        self.language = language.to_string();
        self
    }

    pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError> {
        Ok(Self {
            http,
            cache: CatalogCache::open(catalog_db, now)?,
            language: "zh-CN".to_string(),
        })
    }

    pub fn set_now(&self, now: i64) {
        self.cache.set_now(now);
    }

    pub fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        let path = format!("/search/movie?query={}", form_encode(query));
        self.cached_search(MediaKind::Movie, &path)
    }

    pub fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        let path = format!("/search/tv?query={}", form_encode(query));
        self.cached_search(MediaKind::Tv, &path)
    }

    pub fn popular_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, "/movie/popular")
    }

    pub fn popular_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, "/tv/popular")
    }

    pub fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, "/movie/top_rated")
    }

    pub fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, "/tv/top_rated")
    }

    pub fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, "/movie/now_playing")
    }

    pub fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, "/tv/on_the_air")
    }

    /// Filtered Discover wall. `query` is a TMDB discover query string
    /// (e.g. "with_genres=28&sort_by=vote_average.desc&language=zh-CN").
    pub fn discover_movie(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, &format!("/discover/movie?{query}"))
    }

    pub fn discover_tv(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, &format!("/discover/tv?{query}"))
    }

    /// Paged discover for a collection wall: `(hits, page, total_pages, total_results)`.
    pub fn discover_movie_paged(
        &self,
        query: &str,
        page: i64,
    ) -> Result<(Vec<CatalogHit>, i64, i64, i64), TmdbError> {
        self.paged_search(
            MediaKind::Movie,
            &format!("/discover/movie?{query}&page={page}"),
        )
    }

    pub fn discover_tv_paged(
        &self,
        query: &str,
        page: i64,
    ) -> Result<(Vec<CatalogHit>, i64, i64, i64), TmdbError> {
        self.paged_search(MediaKind::Tv, &format!("/discover/tv?{query}&page={page}"))
    }

    pub fn trending_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, "/trending/movie/day")
    }

    pub fn trending_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, "/trending/tv/day")
    }

    pub fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, "/movie/upcoming")
    }

    pub fn airing_today_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, "/tv/airing_today")
    }

    /// TMDB genre list `[{id, name}]` for the discover filter dialog.
    pub fn genres(&self, kind: MediaKind) -> Result<Vec<(i64, String)>, TmdbError> {
        let path = match kind {
            MediaKind::Movie | MediaKind::Video => "/genre/movie/list",
            MediaKind::Tv => "/genre/tv/list",
        };
        let body = self.body(path)?;
        let parsed: GenreList = serde_json::from_str(&body)?;
        Ok(parsed
            .genres
            .into_iter()
            .map(|genre| (genre.id, genre.name))
            .collect())
    }

    pub fn details_poster(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
    ) -> Result<Option<String>, TmdbError> {
        let path = match kind {
            MediaKind::Movie | MediaKind::Video => format!("/movie/{tmdb_id}"),
            MediaKind::Tv => format!("/tv/{tmdb_id}"),
        };
        parse::details_poster_path(&self.body(&path)?)
    }

    pub fn movie_details(&self, tmdb_id: &str) -> Result<Media, TmdbError> {
        let path = format!("/movie/{tmdb_id}?append_to_response=translations,alternative_titles");
        match self.cached_details(MediaKind::Movie, &path) {
            Ok(media) => Ok(media),
            Err(_) => self.cached_details(MediaKind::Movie, &format!("/movie/{tmdb_id}")),
        }
    }

    pub fn tv_details(&self, tmdb_id: &str) -> Result<Media, TmdbError> {
        let path = format!("/tv/{tmdb_id}?append_to_response=translations,alternative_titles");
        match self.cached_details(MediaKind::Tv, &path) {
            Ok(media) => Ok(media),
            Err(_) => self.cached_details(MediaKind::Tv, &format!("/tv/{tmdb_id}")),
        }
    }

    /// Season overviews from the `/tv/{id}` details body (shares its cache key).
    pub fn tv_seasons(&self, tmdb_id: &str) -> Result<Vec<parse::TvSeason>, TmdbError> {
        let path1 = format!("/tv/{tmdb_id}?append_to_response=translations,alternative_titles");
        match self.body(&path1) {
            Ok(body) => match parse::tv_seasons(&body) {
                Ok(seasons) if !seasons.is_empty() => Ok(seasons),
                Ok(_) => {
                    tracing::warn!(tmdb_id = %tmdb_id, "从 path1 解析出的 seasons 为空");
                    let path2 = format!("/tv/{tmdb_id}");
                    parse::tv_seasons(&self.body(&path2)?)
                }
                Err(e) => {
                    tracing::error!(tmdb_id = %tmdb_id, error = ?e, "从 path1 parse::tv_seasons 报错");
                    let path2 = format!("/tv/{tmdb_id}");
                    parse::tv_seasons(&self.body(&path2)?)
                }
            },
            Err(e) => {
                tracing::error!(tmdb_id = %tmdb_id, error = ?e, "获取 path1 body 报错");
                let path2 = format!("/tv/{tmdb_id}");
                parse::tv_seasons(&self.body(&path2)?)
            }
        }
    }

    /// `/tv/{id}/season/{season}` episodes (episode_number + air_date).
    pub fn season_episodes(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<parse::SeasonEpisode>, TmdbError> {
        let path = format!("/tv/{tmdb_id}/season/{season}");
        parse::season_episodes(&self.body(&path)?)
    }

    /// Details + rich metadata using the client's language, then English fallback,
    /// and the default CN → US → first available certification preference.
    pub fn details_with_meta(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
    ) -> Result<parse::ItemMeta, TmdbError> {
        let languages = [self.language.as_str(), "en-US"];
        self.details_with_meta_for(kind, tmdb_id, &languages, &["CN", "US"])
    }

    /// Fetch rich details with explicit metadata-language and certification priorities.
    /// TMDB's translations are appended to this same response, so missing overview and
    /// tagline values fall back without issuing additional requests.
    pub fn details_with_meta_for(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
        metadata_languages: &[&str],
        rating_countries: &[&str],
    ) -> Result<parse::ItemMeta, TmdbError> {
        let default_languages = [self.language.as_str(), "en-US"];
        let languages = if metadata_languages.is_empty() {
            &default_languages[..]
        } else {
            metadata_languages
        };
        let default_countries = ["CN", "US"];
        let countries = if rating_countries.is_empty() {
            &default_countries[..]
        } else {
            rating_countries
        };
        let request_language = languages
            .iter()
            .copied()
            .find(|language| !language.trim().is_empty())
            .unwrap_or(self.language.as_str());
        let path = match kind {
            MediaKind::Movie | MediaKind::Video => {
                format!("/movie/{tmdb_id}?append_to_response=credits,release_dates,translations")
            }
            // `credits` on TV details only returns the principal cast. The
            // aggregate endpoint includes cast credited across the series. Ratings and
            // translations are subresources, so they must be appended explicitly.
            MediaKind::Tv => format!(
                "/tv/{tmdb_id}?append_to_response=aggregate_credits,content_ratings,translations"
            ),
        };
        parse::details_meta_for(
            &self.body_with_language(&path, request_language)?,
            languages,
            countries,
        )
    }

    /// TMDB configuration countries (cached via the normal body cache).
    pub fn configuration_countries(&self) -> Result<Vec<parse::CountryRow>, TmdbError> {
        parse::configuration_countries(&self.body("/configuration/countries")?)
    }

    /// TMDB configuration languages (cached via the normal body cache).
    pub fn configuration_languages(&self) -> Result<Vec<parse::LanguageRow>, TmdbError> {
        parse::configuration_languages(&self.body("/configuration/languages")?)
    }

    /// Similar titles (`/movie|tv/{id}/similar`), same hit shape as search.
    pub fn similar(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
    ) -> Result<Vec<parse::CatalogHit>, TmdbError> {
        let path = match kind {
            MediaKind::Movie | MediaKind::Video => format!("/movie/{tmdb_id}/similar"),
            MediaKind::Tv => format!("/tv/{tmdb_id}/similar"),
        };
        parse::search_results(kind, &self.body(&path)?)
    }

    /// Whole-season episode details (names/overviews/stills) — one request
    /// instead of per-episode.
    pub fn season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<parse::EpisodeMeta>, TmdbError> {
        self.season_details_lang(tmdb_id, season, None)
    }

    /// Season details with an explicit language (e.g. 季级回落用次语言重拉）；
    /// `lang=None` 用客户端默认语言。
    pub fn season_details_lang(
        &self,
        tmdb_id: &str,
        season: u32,
        lang: Option<&str>,
    ) -> Result<Vec<parse::EpisodeMeta>, TmdbError> {
        let path = format!("/tv/{tmdb_id}/season/{season}");
        let language = lang.unwrap_or(self.language.as_str());
        parse::season_details(&self.body_with_language(&path, language)?)
    }

    /// TMDB episode still file paths (`/tv/{id}/season/{s}/episode/{e}/images`).
    pub fn episode_images(
        &self,
        tmdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Vec<String>, TmdbError> {
        let path = format!("/tv/{tmdb_id}/season/{season}/episode/{episode}/images");
        parse::episode_stills(&self.body_no_language(&path)?)
    }

    /// Raw poster/backdrop candidate rows for an item (`/images`).
    pub fn images(&self, kind: MediaKind, tmdb_id: &str) -> Result<parse::Images, TmdbError> {
        let path = match kind {
            MediaKind::Movie | MediaKind::Video => format!("/movie/{tmdb_id}/images"),
            MediaKind::Tv => format!("/tv/{tmdb_id}/images"),
        };
        parse::images(&self.body_no_language(&path)?)
    }

    /// Person + combined credits (`/person/{id}?append_to_response=combined_credits`).
    pub fn person_details(
        &self,
        tmdb_person_id: &str,
    ) -> Result<crate::person::PersonDetails, TmdbError> {
        let path = format!("/person/{tmdb_person_id}?append_to_response=combined_credits");
        crate::person::person_details(&self.body(&path)?)
    }
    fn cached_search(&self, kind: MediaKind, path: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        parse::search_results(kind, &self.body(path)?)
    }

    fn paged_search(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<(Vec<CatalogHit>, i64, i64, i64), TmdbError> {
        parse::paged_search_results(kind, &self.body(path)?)
    }

    fn cached_details(&self, kind: MediaKind, path: &str) -> Result<Media, TmdbError> {
        parse::details(kind, &self.body(path)?)
    }

    fn body(&self, path: &str) -> Result<String, TmdbError> {
        self.body_with_language(path, &self.language)
    }

    /// Fetch without `language=` — `/images` returns *all* images (any
    /// language) when unqualified; `language=zh-CN` collapses the set to
    /// Chinese-tagged rows only.
    fn body_no_language(&self, path: &str) -> Result<String, TmdbError> {
        self.body_with_language(path, "")
    }

    fn body_with_language(&self, path: &str, language: &str) -> Result<String, TmdbError> {
        // language goes into the cache key, not just the upstream URL, so a
        // locale change misses instead of serving another language's rows.
        let join = if path.contains('?') { '&' } else { '?' };
        let full = if language.is_empty() {
            path.to_string()
        } else {
            format!("{path}{join}language={language}")
        };
        if let Some(cached) = self.cache.get_fresh(&full)? {
            tracing::debug!(path = %full, "TMDB 缓存命中（新鲜）");
            return Ok(cached);
        }
        tracing::debug!(path = %full, "正在从 TMDB API 拉取元数据");
        match self.http.get(&full) {
            Ok(body) => {
                self.cache.put(&full, &body)?;
                Ok(body)
            }
            Err(error) => {
                tracing::warn!(path = %full, error = ?error, "TMDB API 请求失败，尝试过期缓存");
                if let Some(stale) = self.cache.get_stale(&full)? {
                    tracing::info!(path = %full, "回退到过期的元数据缓存");
                    return Ok(stale);
                }
                tracing::error!(path = %full, error = ?error, "TMDB 元数据拉取彻底失败");
                Err(error)
            }
        }
    }
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}
