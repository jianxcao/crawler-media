use serde::Deserialize;

use crate::client::TmdbError;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpisodeMeta {
    pub episode_number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub still_path: Option<String>,
    pub air_date: Option<String>,
    pub runtime_minutes: Option<u32>,
    /// TMDB episode vote_average, rounded to the same one-decimal string as ItemMeta.rating.
    pub rating: Option<String>,
    pub vote_count: Option<u64>,
}

pub fn season_details(body: &str) -> Result<Vec<EpisodeMeta>, TmdbError> {
    #[derive(Deserialize)]
    struct Season {
        #[serde(default)]
        episodes: Vec<EpisodeRow>,
    }
    #[derive(Deserialize)]
    struct EpisodeRow {
        episode_number: u32,
        name: Option<String>,
        overview: Option<String>,
        still_path: Option<String>,
        #[serde(default)]
        air_date: Option<String>,
        #[serde(default)]
        runtime: Option<i64>,
        #[serde(default)]
        vote_average: Option<f64>,
        #[serde(default)]
        vote_count: Option<u64>,
    }
    let parsed: Season = serde_json::from_str(body)?;
    Ok(parsed
        .episodes
        .into_iter()
        .map(|row| EpisodeMeta {
            episode_number: row.episode_number,
            name: row.name.filter(|n| !n.is_empty()),
            overview: row.overview.filter(|o| !o.is_empty()),
            still_path: row.still_path.filter(|p| !p.is_empty()),
            air_date: row.air_date.filter(|date| !date.is_empty()),
            runtime_minutes: row.runtime.and_then(|runtime| u32::try_from(runtime).ok()),
            rating: row.vote_average.map(|rating| format!("{rating:.1}")),
            vote_count: row.vote_count,
        })
        .collect())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemMeta {
    pub overview: Option<String>,
    pub rating: Option<String>,
    pub runtime_minutes: Option<String>,
    /// Movie release date or TV first-air date, as returned by TMDB.
    pub release_date: Option<String>,
    pub last_air_date: Option<String>,
    /// Production companies for movies, broadcast networks for TV series.
    pub studios: Vec<String>,
    /// Certification selected in CN → US → first available order.
    pub content_rating: Option<String>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub vote_count: Option<u64>,
    pub tagline: Option<String>,
    pub number_of_seasons: Option<u32>,
    pub number_of_episodes: Option<u32>,
    pub episode_run_time: Vec<u32>,
    pub directors: Vec<String>,
    pub creators: Vec<String>,
    pub genres: Vec<String>,
    pub genre_ids: Vec<i64>,
    pub origin_countries: Vec<String>,
    pub cast: Vec<CastRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CastRow {
    pub name: String,
    pub role: Option<String>,
    /// TMDB `/person/{id}` reference (clickable to the person page).
    pub person_id: Option<i64>,
    /// TMDB profile file path (`/abc.jpg`), used for the avatar.
    pub avatar_path: Option<String>,
    /// TMDB cast order, falling back to the row's response position when omitted.
    pub order: u32,
}

#[derive(Deserialize)]
struct DetailsMeta {
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    tagline: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    vote_count: Option<u64>,
    #[serde(default)]
    runtime: Option<u32>,
    #[serde(default)]
    episode_run_time: Vec<i64>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    last_air_date: Option<String>,
    #[serde(default)]
    original_language: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    number_of_seasons: Option<i64>,
    #[serde(default)]
    number_of_episodes: Option<i64>,
    #[serde(default)]
    genres: Option<Vec<GenreRow>>,
    #[serde(default)]
    origin_country: Vec<String>,
    credits: Option<Credits>,
    aggregate_credits: Option<Credits>,
    #[serde(default)]
    created_by: Vec<NameRow>,
    #[serde(default)]
    production_companies: Vec<NameRow>,
    #[serde(default)]
    networks: Vec<NameRow>,
    #[serde(default)]
    release_dates: Option<ReleaseDates>,
    #[serde(default)]
    content_ratings: Option<ContentRatings>,
    #[serde(default)]
    translations: Option<TranslationsWrapper>,
}

#[derive(Deserialize)]
struct TranslationsWrapper {
    #[serde(default)]
    translations: Vec<TranslationItem>,
}

#[derive(Deserialize)]
struct TranslationItem {
    iso_639_1: Option<String>,
    #[serde(default)]
    iso_3166_1: Option<String>,
    #[serde(default)]
    data: TranslationData,
}

#[derive(Deserialize, Default)]
struct TranslationData {
    overview: Option<String>,
    tagline: Option<String>,
}

#[derive(Deserialize)]
struct NameRow {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseDates {
    #[serde(default)]
    results: Vec<ReleaseCountry>,
}

#[derive(Deserialize)]
struct ReleaseCountry {
    iso_3166_1: Option<String>,
    #[serde(default)]
    release_dates: Vec<ReleaseDateRow>,
}

#[derive(Deserialize)]
struct ReleaseDateRow {
    certification: Option<String>,
}

#[derive(Deserialize)]
struct ContentRatings {
    #[serde(default)]
    results: Vec<ContentRatingRow>,
}

#[derive(Deserialize)]
struct ContentRatingRow {
    iso_3166_1: Option<String>,
    rating: Option<String>,
}

#[derive(Deserialize)]
struct GenreRow {
    #[serde(default)]
    id: Option<i64>,
    name: String,
}

#[derive(Deserialize, Default)]
struct Credits {
    #[serde(default)]
    cast: Vec<CastRowRaw>,
    #[serde(default)]
    crew: Vec<CrewRowRaw>,
}

#[derive(Clone, Deserialize)]
struct CastRowRaw {
    name: Option<String>,
    character: Option<String>,
    #[serde(default)]
    roles: Vec<CastRoleRaw>,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    profile_path: Option<String>,
    #[serde(default)]
    order: Option<u32>,
}

#[derive(Deserialize)]
struct CrewRowRaw {
    name: Option<String>,
    job: Option<String>,
}

#[derive(Clone, Deserialize)]
struct CastRoleRaw {
    character: Option<String>,
}

/// Parse item metadata using caller-provided language and certification priorities.
pub fn details_meta_for(
    body: &str,
    metadata_languages: &[&str],
    rating_countries: &[&str],
) -> Result<ItemMeta, TmdbError> {
    let parsed: DetailsMeta = serde_json::from_str(body)?;
    let aggregate_cast = parsed
        .aggregate_credits
        .as_ref()
        .filter(|credits| !credits.cast.is_empty());
    let include_all_cast = aggregate_cast.is_some();
    let cast_rows = aggregate_cast
        .map(|credits| credits.cast.clone())
        .unwrap_or_else(|| {
            parsed
                .credits
                .as_ref()
                .map(|credits| credits.cast.clone())
                .unwrap_or_default()
        });
    let crew = parsed.credits.as_ref().map(|credits| &credits.crew);
    let directors = crew
        .into_iter()
        .flatten()
        .filter(|credit| credit.job.as_deref() == Some("Director"))
        .filter_map(|credit| credit.name.clone().filter(|name| !name.is_empty()))
        .collect();
    let studios = if parsed.networks.is_empty() {
        parsed.production_companies.iter()
    } else {
        parsed.networks.iter()
    }
    .filter_map(|row| row.name.clone().filter(|name| !name.is_empty()))
    .collect();
    let episode_run_time: Vec<u32> = parsed
        .episode_run_time
        .iter()
        .filter_map(|runtime| u32::try_from(*runtime).ok())
        .collect();
    let runtime_minutes = parsed
        .runtime
        .or_else(|| episode_run_time.first().copied())
        .map(|runtime| runtime.to_string());
    let overview = parsed
        .overview
        .clone()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| translated_text(&parsed, metadata_languages, TranslationField::Overview));
    let tagline = parsed
        .tagline
        .clone()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| translated_text(&parsed, metadata_languages, TranslationField::Tagline));
    let content_rating = selected_content_rating(&parsed, rating_countries);
    let parsed_genres = parsed.genres.unwrap_or_default();
    let genres: Vec<String> = parsed_genres
        .iter()
        .map(|g| g.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    let genre_ids: Vec<i64> = parsed_genres.iter().filter_map(|g| g.id).collect();
    Ok(ItemMeta {
        overview,
        rating: parsed.vote_average.map(|v| format!("{v:.1}")),
        runtime_minutes,
        release_date: parsed
            .release_date
            .or(parsed.first_air_date)
            .filter(|date| !date.is_empty()),
        last_air_date: parsed.last_air_date.filter(|date| !date.is_empty()),
        studios,
        content_rating,
        original_language: parsed
            .original_language
            .filter(|language| !language.is_empty()),
        status: parsed.status.filter(|status| !status.is_empty()),
        vote_count: parsed.vote_count,
        tagline,
        number_of_seasons: parsed
            .number_of_seasons
            .and_then(|count| u32::try_from(count).ok()),
        number_of_episodes: parsed
            .number_of_episodes
            .and_then(|count| u32::try_from(count).ok()),
        episode_run_time,
        directors,
        creators: parsed
            .created_by
            .into_iter()
            .filter_map(|row| row.name.filter(|name| !name.is_empty()))
            .collect(),
        genres,
        genre_ids,
        origin_countries: parsed.origin_country,
        cast: parse_cast_rows(cast_rows, include_all_cast),
    })
}

fn translated_text(
    details: &DetailsMeta,
    priorities: &[&str],
    field: TranslationField,
) -> Option<String> {
    let translations = &details.translations.as_ref()?.translations;
    for priority in priorities
        .iter()
        .filter(|language| !language.trim().is_empty())
    {
        let (language, region) = priority.split_once('-').unwrap_or((priority, ""));
        let exact = translations.iter().find(|row| {
            row.iso_639_1.as_deref() == Some(language)
                && !region.is_empty()
                && row.iso_3166_1.as_deref() == Some(region)
        });
        if let Some(value) = exact
            .and_then(|row| translation_value(&row.data, field))
            .filter(|value| !value.trim().is_empty())
        {
            return Some(value.trim().to_string());
        }
        if let Some(value) = translations
            .iter()
            .filter(|row| row.iso_639_1.as_deref() == Some(language))
            .find_map(|row| translation_value(&row.data, field))
            .filter(|value| !value.trim().is_empty())
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

#[derive(Clone, Copy)]
enum TranslationField {
    Overview,
    Tagline,
}

fn translation_value(data: &TranslationData, field: TranslationField) -> Option<&str> {
    match field {
        TranslationField::Overview => data.overview.as_deref(),
        TranslationField::Tagline => data.tagline.as_deref(),
    }
}

fn selected_content_rating(details: &DetailsMeta, priorities: &[&str]) -> Option<String> {
    let mut ratings = Vec::new();
    if let Some(release_dates) = &details.release_dates {
        for country in &release_dates.results {
            if let Some(certification) = country
                .release_dates
                .iter()
                .filter_map(|release| release.certification.as_deref())
                .find(|certification| !certification.trim().is_empty())
            {
                ratings.push((country.iso_3166_1.as_deref(), certification.to_string()));
            }
        }
    }
    if let Some(content_ratings) = &details.content_ratings {
        ratings.extend(content_ratings.results.iter().filter_map(|row| {
            let rating = row.rating.as_deref()?.trim();
            (!rating.is_empty()).then(|| (row.iso_3166_1.as_deref(), rating.to_string()))
        }));
    }
    priorities
        .iter()
        .find_map(|region| {
            ratings
                .iter()
                .find(|(country, _)| *country == Some(*region))
                .map(|(_, rating)| rating.clone())
        })
        .or_else(|| ratings.first().map(|(_, rating)| rating.clone()))
}

fn parse_cast_rows(rows: Vec<CastRowRaw>, include_all: bool) -> Vec<CastRow> {
    let mut cast: Vec<CastRow> = rows
        .into_iter()
        .enumerate()
        .filter_map(|(position, credit)| {
            credit
                .name
                .filter(|name| !name.is_empty())
                .map(|name| CastRow {
                    name,
                    role: cast_role(credit.character, credit.roles),
                    person_id: credit.id,
                    avatar_path: credit.profile_path.filter(|path| !path.is_empty()),
                    order: credit.order.unwrap_or(position as u32),
                })
        })
        .collect();
    cast.sort_by_key(|credit| credit.order);
    if !include_all {
        cast.truncate(20);
    }
    cast
}

fn cast_role(direct_role: Option<String>, roles: Vec<CastRoleRaw>) -> Option<String> {
    let direct_role = direct_role.filter(|role| !role.is_empty());
    if direct_role.is_some() {
        return direct_role;
    }

    let mut role_names = Vec::new();
    for role in roles.into_iter().filter_map(|role| role.character) {
        if !role.is_empty() && !role_names.contains(&role) {
            role_names.push(role);
        }
    }
    (!role_names.is_empty()).then(|| role_names.join(" / "))
}

#[derive(Deserialize)]
struct TvSeasonRow {
    name: Option<String>,
    season_number: i64,
    #[serde(default)]
    episode_count: Option<i64>,
    #[serde(default)]
    air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

/// 一集（season/episode 列表，用于订阅单元的播出日期）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeasonEpisode {
    pub episode_number: u32,
    pub air_date: Option<String>,
}

/// One season from the `/tv/{id}` details body (used by subscription preview).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TvSeason {
    pub season_number: u32,
    pub name: String,
    pub episode_count: Option<u32>,
    pub air_date: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
}

/// `/tv/{id}/season/{s}` → (episode_number, air_date) 列表。
pub fn season_episodes(body: &str) -> Result<Vec<SeasonEpisode>, TmdbError> {
    #[derive(Deserialize)]
    struct EpisodesBody {
        #[serde(default)]
        episodes: Vec<EpisodeRow>,
    }
    #[derive(Deserialize)]
    struct EpisodeRow {
        episode_number: i64,
        #[serde(default)]
        air_date: Option<String>,
    }
    let parsed: EpisodesBody = serde_json::from_str(body)?;
    Ok(parsed
        .episodes
        .into_iter()
        .filter(|row| row.episode_number > 0)
        .map(|row| SeasonEpisode {
            episode_number: row.episode_number as u32,
            air_date: row.air_date.filter(|d| !d.is_empty()),
        })
        .collect())
}

pub fn tv_seasons(body: &str) -> Result<Vec<TvSeason>, TmdbError> {
    #[derive(Deserialize)]
    struct SeasonsBody {
        #[serde(default)]
        seasons: Vec<TvSeasonRow>,
    }
    let parsed: SeasonsBody = serde_json::from_str(body)?;
    tracing::info!(
        seasons_count = parsed.seasons.len(),
        "parse::tv_seasons 正在解析"
    );
    for s in &parsed.seasons {
        tracing::info!(season_number = s.season_number, name = ?s.name, ep_count = ?s.episode_count, "解析单季");
    }
    Ok(parsed
        .seasons
        .into_iter()
        .filter(|row| row.season_number > 0)
        .map(|row| TvSeason {
            season_number: row.season_number as u32,
            name: row.name.unwrap_or_default(),
            episode_count: row.episode_count.and_then(|n| u32::try_from(n).ok()),
            air_date: row.air_date,
            overview: row.overview.filter(|overview| !overview.is_empty()),
            poster_path: row.poster_path.filter(|path| !path.is_empty()),
        })
        .collect())
}
