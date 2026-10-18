use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use domain::{EntityScope, MediaId, MetadataAdoption};
use super::types::EffectiveMetadata;
use super::super::{Store, StoreError};

impl Store {
    pub fn adopt_metadata(
        &self,
        input: &MetadataAdoption,
        _now_ms: i64,
    ) -> Result<i64, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let media_id_str = input.media_id.to_string();
        let (season, episode) = input.scope.season_episode();

        // 1. Serialize fields and origins
        let normalized_json = serde_json::to_string(&input.normalized.fields)?;
        let present_fields_json = serde_json::to_string(&input.normalized.field_origins.keys().collect::<Vec<_>>())?;
        let preferences_json = serde_json::to_string(&input.preferences)?;
        let selection_origins_json = serde_json::to_string(&input.normalized.field_origins)?;

        // 2. Fetch or bump source revision
        let cur_source_rev: Option<i64> = tx
            .query_row(
                "SELECT source_revision FROM media_metadata_sources
                 WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND source = ?5 AND locale = ?6",
                params![
                    media_id_str,
                    input.scope.as_str(),
                    season,
                    episode,
                    input.source,
                    input.locale,
                ],
                |r| r.get(0),
            )
            .optional()?;

        let next_source_rev = cur_source_rev.unwrap_or(0) + 1;

        tx.execute(
            "INSERT INTO media_metadata_sources
                (media_id, entity_scope, season, episode, source, locale, provider_entity_id,
                 provider_work_id, normalized_json, present_fields_json, preferences_json,
                 selection_origins_json, fetched_at, source_revision)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(media_id, entity_scope, season, episode, source, locale) DO UPDATE SET
                 provider_entity_id = excluded.provider_entity_id,
                 provider_work_id = excluded.provider_work_id,
                 normalized_json = excluded.normalized_json,
                 present_fields_json = excluded.present_fields_json,
                 preferences_json = excluded.preferences_json,
                 selection_origins_json = excluded.selection_origins_json,
                 fetched_at = excluded.fetched_at,
                 source_revision = excluded.source_revision",
            params![
                media_id_str,
                input.scope.as_str(),
                season,
                episode,
                input.source,
                input.locale,
                input.provider_entity_id,
                input.provider_work_id,
                normalized_json,
                present_fields_json,
                preferences_json,
                selection_origins_json,
                input.fetched_at,
                next_source_rev,
            ],
        )?;

        // 3. Update effective table based on scope
        let next_eff_rev = match input.scope {
            EntityScope::Work => {
                let cur_rev: Option<i64> = tx
                    .query_row(
                        "SELECT revision FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
                        params![media_id_str, input.locale],
                        |r| r.get(0),
                    )
                    .optional()?;
                let rev = cur_rev.unwrap_or(0) + 1;
                let title = input.normalized.fields.title.value().cloned().unwrap_or_default();
                let original_title = input.normalized.fields.original_title.value().cloned();
                let year = input.normalized.fields.year.value().copied().map(|y| y as i64);
                let overview = input.normalized.fields.overview.value().cloned();
                let release_date = input.normalized.fields.release_date.value().cloned();
                let rating = input.normalized.fields.rating.value().copied();
                let runtime = input.normalized.fields.runtime_minutes.value().copied().map(|r| r as i64);
                let language = input.normalized.fields.language.value().cloned();
                let empty_genres: Vec<domain::MetadataGenre> = Vec::new();
                let genres_ref = input.normalized.fields.genres.value().unwrap_or(&empty_genres);
                let genres_json = serde_json::to_string(genres_ref)?;
                let poster_url = input.normalized.fields.poster_url.value().cloned();
                let backdrop_url = input.normalized.fields.backdrop_url.value().cloned();
                tx.execute(
                    "INSERT INTO media_metadata
                        (media_id, locale, title, original_title, year, overview, release_date,
                         rating, runtime_minutes, language, genres_json, countries_json, poster_url,
                         backdrop_url, details_json, field_origins_json, revision)
                     VALUES
                        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, '[]', ?12, ?13, '{}', ?14, ?15)
                     ON CONFLICT(media_id, locale) DO UPDATE SET
                         title = excluded.title,
                         original_title = excluded.original_title,
                         year = excluded.year,
                         overview = excluded.overview,
                         release_date = excluded.release_date,
                         rating = excluded.rating,
                         runtime_minutes = excluded.runtime_minutes,
                         language = excluded.language,
                         genres_json = excluded.genres_json,
                         poster_url = excluded.poster_url,
                         backdrop_url = excluded.backdrop_url,
                         field_origins_json = excluded.field_origins_json,
                         revision = excluded.revision",
                    params![
                        media_id_str,
                        input.locale,
                        title,
                        original_title,
                        year,
                        overview,
                        release_date,
                        rating,
                        runtime,
                        language,
                        genres_json,
                        poster_url,
                        backdrop_url,
                        selection_origins_json,
                        rev,
                    ],
                )?;

                // Also update canonical media title/year if present
                if !title.is_empty() {
                    tx.execute(
                        "UPDATE media SET title = ?1 WHERE media_id = ?2",
                        params![title, media_id_str],
                    )?;
                }
                if let Some(y) = year {
                    tx.execute(
                        "UPDATE media SET year = ?1 WHERE media_id = ?2",
                        params![y, media_id_str],
                    )?;
                }

                rev
            }
            EntityScope::Season(s) => {
                let title = input.normalized.fields.title.value().cloned();
                let overview = input.normalized.fields.overview.value().cloned();
                let air_date = input.normalized.fields.air_date.value().cloned();
                let poster_url = input.normalized.fields.poster_url.value().cloned();
                tx.execute(
                    "INSERT INTO media_seasons
                        (media_id, season, locale, title, overview, air_date, poster_url, details_json, field_origins_json, revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', ?8, 1)
                     ON CONFLICT(media_id, season, locale) DO UPDATE SET
                         title = excluded.title,
                         overview = excluded.overview,
                         air_date = excluded.air_date,
                         poster_url = excluded.poster_url,
                         field_origins_json = excluded.field_origins_json,
                         revision = media_seasons.revision + 1",
                    params![media_id_str, s as i64, input.locale, title, overview, air_date, poster_url, selection_origins_json],
                )?;
                1
            }
            EntityScope::Episode { season: s, episode: ep } => {
                let title = input.normalized.fields.title.value().cloned();
                let overview = input.normalized.fields.overview.value().cloned();
                let air_date = input.normalized.fields.air_date.value().cloned();
                let runtime = input.normalized.fields.runtime_minutes.value().copied().map(|r| r as i64);
                let rating = input.normalized.fields.rating.value().copied();
                let still_url = input.normalized.fields.still_url.value().cloned();
                tx.execute(
                    "INSERT INTO media_episodes
                        (media_id, season, episode, locale, title, overview, air_date, runtime_minutes, rating, still_url, details_json, field_origins_json, revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '{}', ?11, 1)
                     ON CONFLICT(media_id, season, episode, locale) DO UPDATE SET
                         title = excluded.title,
                         overview = excluded.overview,
                         air_date = excluded.air_date,
                         runtime_minutes = excluded.runtime_minutes,
                         rating = excluded.rating,
                         still_url = excluded.still_url,
                         field_origins_json = excluded.field_origins_json,
                         revision = media_episodes.revision + 1",
                    params![media_id_str, s as i64, ep as i64, input.locale, title, overview, air_date, runtime, rating, still_url, selection_origins_json],
                )?;
                1
            }
        };

        tx.commit()?;
        Ok(next_eff_rev)
    }

    pub fn get_effective_metadata(
        &self,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<EffectiveMetadata>, StoreError> {
        let sql = "SELECT media_id, locale, title, original_title, year, overview, release_date,
                          rating, runtime_minutes, language, genres_json, poster_url, backdrop_url,
                          details_json, field_origins_json, revision
                   FROM media_metadata
                   WHERE media_id = ?1 AND locale = ?2";
        let mut stmt = self.library.prepare(sql)?;
        let row = stmt
            .query_row(params![media_id.to_string(), locale], |r| {
                let y: Option<i64> = r.get(4)?;
                let rt: Option<i64> = r.get(8)?;
                Ok(EffectiveMetadata {
                    media_id,
                    locale: r.get(1)?,
                    title: r.get(2)?,
                    original_title: r.get(3)?,
                    year: y.map(|v| v as u16),
                    overview: r.get(5)?,
                    release_date: r.get(6)?,
                    rating: r.get(7)?,
                    runtime_minutes: rt.map(|v| v as u32),
                    language: r.get(9)?,
                    genres_json: r.get(10)?,
                    poster_url: r.get(11)?,
                    backdrop_url: r.get(12)?,
                    details_json: r.get(13)?,
                    field_origins_json: r.get(14)?,
                    revision: r.get(15)?,
                })
            })
            .optional()?;
        Ok(row)
    }
}
