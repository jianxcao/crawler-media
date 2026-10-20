use rusqlite::{Connection, OptionalExtension, params};

use domain::{EntityScope, MediaId, MetadataAdoption};
use super::types::EffectiveMetadata;
use super::super::{Store, StoreError};

/// Fixed adoption priority: a lower rank wins a field for the same entity/locale.
fn source_rank(source: &str) -> u8 {
    match source {
        "tmdb" => 0,
        "douban" => 1,
        "tvdb" => 2,
        "bangumi" => 3,
        "anilist" => 4,
        "fanart" => 9,
        _ => 5,
    }
}

/// Per-field rank of the highest-priority source that already provided a value.
fn existing_field_ranks(
    tx: &Connection,
    media_id: &str,
    scope: EntityScope,
    locale: &str,
) -> Result<std::collections::BTreeMap<String, u8>, StoreError> {
    let (scope_name, season, episode) = (scope.as_str(), scope.season_episode().0, scope.season_episode().1);
    let mut stmt = tx.prepare(
        "SELECT source, normalized_json FROM media_metadata_sources
         WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4
           AND locale = ?5",
    )?;
    let rows = stmt.query_map(
        params![media_id, scope_name, season, episode, locale],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut ranks = std::collections::BTreeMap::new();
    for row in rows {
        let (source, payload) = row?;
        let rank = source_rank(&source);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
            continue;
        };
        let fields = value.get("fields").unwrap_or(&value);
        let Some(map) = fields.as_object() else { continue };
        for (field, field_value) in map {
            if field_value.get("state").and_then(|state| state.as_str()) == Some("missing") {
                continue;
            }
            ranks
                .entry(field.clone())
                .and_modify(|current: &mut u8| *current = (*current).min(rank))
                .or_insert(rank);
        }
    }
    Ok(ranks)
}

/// Merge one source's normalized fields over the already-adopted fields.
///
/// A field is replaced only when the incoming source has at least the rank of the
/// source that currently owns it, so a later low-priority adoption cannot overwrite
/// a higher-priority value. Missing incoming fields never erase an existing value;
/// an explicit `Empty` is a real value and follows the same rank rule.
fn merge_by_source_priority(
    existing_details_json: Option<&str>,
    owner_ranks: &std::collections::BTreeMap<String, u8>,
    incoming: &domain::MetadataFields,
    incoming_source: &str,
) -> Result<domain::MetadataFields, StoreError> {
    // Start from a complete field set so a partially populated row still deserializes.
    let mut merged = serde_json::to_value(domain::MetadataFields::default())?;
    if let Some(existing) = existing_details_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("fields").cloned())
        .and_then(|value| value.as_object().cloned())
    {
        if let Some(merged_map) = merged.as_object_mut() {
            for (field, value) in existing {
                merged_map.insert(field, value);
            }
        }
    }
    let incoming_value = serde_json::to_value(incoming)?;
    let incoming_rank = source_rank(incoming_source);
    if let (Some(merged_map), Some(incoming_map)) =
        (merged.as_object_mut(), incoming_value.as_object())
    {
        for (field, value) in incoming_map {
            if value.get("state").and_then(|state| state.as_str()) == Some("missing") {
                continue;
            }
            if owner_ranks.get(field).is_some_and(|rank| *rank < incoming_rank) {
                continue;
            }
            merged_map.insert(field.clone(), value.clone());
        }
    }
    Ok(serde_json::from_value(merged)?)
}

/// Display facts a **Scrape** observed for one **Media**; they are written with the
/// adopted metadata so the card and the detail view read one source of truth.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrapedDisplay<'a> {
    pub rating: Option<f64>,
    pub release_date: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub studio_logo: Option<&'a str>,
}

impl Store {
    pub fn adopt_metadata(
        &self,
        input: &MetadataAdoption,
        _now_ms: i64,
    ) -> Result<i64, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let media_id_str = input.media_id.to_string();
        let (season, episode) = input.scope.season_episode();
        // Who already owns each field, before this source is recorded: a lower-priority
        // source must not overwrite a value adopted from a higher-priority one.
        let owner_ranks = existing_field_ranks(&tx, &media_id_str, input.scope, &input.locale)?;

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
                let existing_details: Option<String> = tx
                    .query_row(
                        "SELECT details_json FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
                        params![media_id_str, input.locale],
                        |r| r.get(0),
                    )
                    .optional()?;
                let fields = merge_by_source_priority(
                    existing_details.as_deref(),
                    &owner_ranks,
                    &input.normalized.fields,
                    &input.source,
                )?;
                let title = fields.title.value().cloned().unwrap_or_default();
                let original_title = fields.original_title.value().cloned();
                let year = fields.year.value().copied().map(|y| y as i64);
                let overview = fields.overview.value().cloned();
                let release_date = fields.release_date.value().cloned();
                let rating = fields.rating.value().copied();
                let runtime = fields.runtime_minutes.value().copied().map(|r| r as i64);
                let language = fields.language.value().cloned();
                let empty_genres: Vec<domain::MetadataGenre> = Vec::new();
                let genres_ref = fields.genres.value().unwrap_or(&empty_genres);
                let genres_json = serde_json::to_string(genres_ref)?;
                let poster_url = fields.poster_url.value().cloned();
                let backdrop_url = fields.backdrop_url.value().cloned();
                let countries_json = match fields.countries.value() {
                    Some(list) => serde_json::to_string(list)?,
                    None => "[]".to_string(),
                };
                let details_json = serde_json::to_string(&serde_json::json!({
                    "version": 1,
                    "fields": fields,
                }))?;
                tx.execute(
                    "INSERT INTO media_metadata
                        (media_id, locale, title, original_title, year, overview, release_date,
                         rating, runtime_minutes, language, genres_json, countries_json, poster_url,
                         backdrop_url, details_json, field_origins_json, revision)
                     VALUES
                        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?16, ?12, ?13, ?17, ?14, ?15)
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
                        countries_json = excluded.countries_json,
                        poster_url = excluded.poster_url,
                        details_json = excluded.details_json,
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
                        countries_json,
                        details_json,
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

        // The card is derived from the facts just written; refresh it in the same transaction.
        crate::library_write::reduce::reduce_media_everywhere(&tx, input.media_id)?;
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

    /// Record display facts observed by a Scrape (rating, release date, studio, logo).
    ///
    /// They belong to the adopted metadata, never inside `library_items.aliases_json`
    /// (which holds only the five external ids), and the affected cards are reduced in
    /// the same transaction.
    pub fn record_scraped_display(
        &self,
        media_id: MediaId,
        locale: &str,
        display: ScrapedDisplay<'_>,
    ) -> Result<(), StoreError> {
        let rating = display.rating.filter(|value| (0.0..=10.0).contains(value));
        let release_date = display.release_date.filter(|value| !value.trim().is_empty());
        let studio = display.studio.filter(|value| !value.trim().is_empty());
        let studio_logo = display.studio_logo.filter(|value| !value.trim().is_empty());
        if rating.is_none() && release_date.is_none() && studio.is_none() && studio_logo.is_none() {
            return Ok(());
        }
        let tx = self.library.unchecked_transaction()?;
        let media = media_id.to_string();
        let existing: Option<String> = tx
            .query_row(
                "SELECT details_json FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
                params![media, locale],
                |r| r.get(0),
            )
            .optional()?;
        let mut payload = existing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        payload.insert("version".into(), serde_json::Value::from(1));
        let fields = payload
            .entry("fields")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(object) = fields.as_object_mut() {
            if let Some(name) = studio {
                object.insert(
                    "studios".into(),
                    serde_json::Value::Array(vec![serde_json::Value::String(name.to_string())]),
                );
            }
            if let Some(logo) = studio_logo {
                object.insert(
                    "studio_logo".into(),
                    serde_json::Value::String(logo.to_string()),
                );
            }
        }
        let details_json = serde_json::Value::Object(payload).to_string();
        match existing {
            Some(_) => {
                tx.execute(
                    "UPDATE media_metadata SET
                         rating = COALESCE(?3, rating),
                         release_date = COALESCE(?4, release_date),
                         details_json = ?5
                     WHERE media_id = ?1 AND locale = ?2",
                    params![media, locale, rating, release_date, details_json],
                )?;
            }
            None => {
                let title: String = tx.query_row(
                    "SELECT title FROM media WHERE media_id = ?1",
                    params![media],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO media_metadata
                        (media_id, locale, title, rating, release_date, genres_json, countries_json,
                         details_json, field_origins_json, revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, '[]', '[]', ?6, '{}', 0)",
                    params![media, locale, title, rating, release_date, details_json],
                )?;
            }
        }
        crate::library_write::reduce::reduce_media_everywhere(&tx, media_id)?;
        tx.commit()?;
        Ok(())
    }
}
