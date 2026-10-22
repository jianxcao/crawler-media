//! Persist the composed effective row of one entity/locale. Called inside the adopting or
//! overriding transaction, so the effective columns, their origins and the cards derived from
//! them are always recomputed from every current snapshot plus the overrides — never patched
//! on top of the previous effective value.
use domain::{EntityScope, MediaId, MetadataFields};
use rusqlite::{Connection, OptionalExtension, params};

use super::compose::{FieldOrigins, compose_entity};
use crate::StoreError;

/// Recompose and write the effective row for `scope`/`locale`; returns its new revision.
pub(crate) fn write_effective(
    tx: &Connection,
    media: MediaId,
    scope: EntityScope,
    locale: &str,
) -> Result<i64, StoreError> {
    let (fields, origins) = compose_entity(tx, media, scope, locale)?;
    match scope {
        EntityScope::Work => write_work(tx, media, locale, &fields, &origins),
        EntityScope::Season(season) => write_season(tx, media, season, locale, &fields, &origins),
        EntityScope::Episode { season, episode } => {
            write_episode(tx, media, (season, episode), locale, &fields, &origins)
        }
    }
}

/// Display-only keys recorded outside any source snapshot (see `record_scraped_display`).
const DISPLAY_ONLY_KEYS: [&str; 1] = ["studio_logo"];

fn work_details_json(
    tx: &Connection,
    media: &str,
    locale: &str,
    fields: &MetadataFields,
) -> Result<String, StoreError> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT details_json FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
            params![media, locale],
            |r| r.get(0),
        )
        .optional()?;
    let mut fields_value = serde_json::to_value(fields)?;
    let previous = existing
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    if let (Some(map), Some(previous)) = (
        fields_value.as_object_mut(),
        previous.as_ref().and_then(|v| v.get("fields")),
    ) {
        for key in DISPLAY_ONLY_KEYS {
            if let Some(value) = previous.get(key) {
                map.insert(key.into(), value.clone());
            }
        }
    }
    Ok(serde_json::json!({ "version": 1, "fields": fields_value }).to_string())
}

fn write_work(
    tx: &Connection,
    media_id: MediaId,
    locale: &str,
    fields: &MetadataFields,
    origins: &FieldOrigins,
) -> Result<i64, StoreError> {
    let media = media_id.to_string();
    let revision: i64 = tx
        .query_row(
            "SELECT revision FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
            params![media, locale],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0)
        + 1;
    let base_title: String = tx.query_row(
        "SELECT title FROM media WHERE media_id = ?1",
        params![media],
        |r| r.get(0),
    )?;
    let adopted_title = fields.title.value().cloned().filter(|t| !t.is_empty());
    let title = adopted_title.clone().unwrap_or(base_title);
    let year = fields.year.value().map(|y| i64::from(*y));
    let genres = serde_json::to_string(fields.genres.value().map_or(&[][..], Vec::as_slice))?;
    let countries = serde_json::to_string(fields.countries.value().map_or(&[][..], Vec::as_slice))?;
    let details = work_details_json(tx, &media, locale, fields)?;
    tx.execute(
        "INSERT INTO media_metadata
            (media_id, locale, title, original_title, year, overview, release_date, rating,
             runtime_minutes, language, genres_json, countries_json, poster_url, backdrop_url,
             details_json, field_origins_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(media_id, locale) DO UPDATE SET
             title = excluded.title, original_title = excluded.original_title,
             year = excluded.year, overview = excluded.overview,
             release_date = excluded.release_date, rating = excluded.rating,
             runtime_minutes = excluded.runtime_minutes, language = excluded.language,
             genres_json = excluded.genres_json, countries_json = excluded.countries_json,
             poster_url = excluded.poster_url, backdrop_url = excluded.backdrop_url,
             details_json = excluded.details_json,
             field_origins_json = excluded.field_origins_json, revision = excluded.revision",
        params![
            media,
            locale,
            title,
            fields.original_title.value(),
            year,
            fields.overview.value(),
            fields.release_date.value(),
            fields
                .rating
                .value()
                .copied()
                .filter(|r| (0.0..=10.0).contains(r)),
            fields.runtime_minutes.value().map(|r| i64::from(*r)),
            fields.language.value(),
            genres,
            countries,
            fields.poster_url.value(),
            fields.backdrop_url.value(),
            details,
            serde_json::to_string(origins)?,
            revision,
        ],
    )?;
    if let Some(title) = adopted_title {
        tx.execute(
            "UPDATE media SET title = ?1 WHERE media_id = ?2",
            params![title, media],
        )?;
    }
    if let Some(year) = year {
        tx.execute(
            "UPDATE media SET year = ?1 WHERE media_id = ?2",
            params![year, media],
        )?;
    }
    Ok(revision)
}

fn write_season(
    tx: &Connection,
    media: MediaId,
    season: u32,
    locale: &str,
    fields: &MetadataFields,
    origins: &FieldOrigins,
) -> Result<i64, StoreError> {
    let details = serde_json::json!({ "version": 1, "fields": fields }).to_string();
    let revision: i64 = tx.query_row(
        "INSERT INTO media_seasons
            (media_id, season, locale, title, overview, air_date, poster_url, details_json,
             field_origins_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
         ON CONFLICT(media_id, season, locale) DO UPDATE SET
             title = excluded.title, overview = excluded.overview, air_date = excluded.air_date,
             poster_url = excluded.poster_url, details_json = excluded.details_json,
             field_origins_json = excluded.field_origins_json,
             revision = media_seasons.revision + 1
         RETURNING revision",
        params![
            media.to_string(),
            i64::from(season),
            locale,
            fields.title.value(),
            fields.overview.value(),
            fields.air_date.value(),
            fields.poster_url.value(),
            details,
            serde_json::to_string(origins)?,
        ],
        |r| r.get(0),
    )?;
    Ok(revision)
}

fn write_episode(
    tx: &Connection,
    media: MediaId,
    (season, episode): (u32, u32),
    locale: &str,
    fields: &MetadataFields,
    origins: &FieldOrigins,
) -> Result<i64, StoreError> {
    let details = serde_json::json!({ "version": 1, "fields": fields }).to_string();
    let revision: i64 = tx.query_row(
        "INSERT INTO media_episodes
            (media_id, season, episode, locale, title, overview, air_date, runtime_minutes,
             rating, still_url, details_json, field_origins_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)
         ON CONFLICT(media_id, season, episode, locale) DO UPDATE SET
             title = excluded.title, overview = excluded.overview, air_date = excluded.air_date,
             runtime_minutes = excluded.runtime_minutes, rating = excluded.rating,
             still_url = excluded.still_url, details_json = excluded.details_json,
             field_origins_json = excluded.field_origins_json,
             revision = media_episodes.revision + 1
         RETURNING revision",
        params![
            media.to_string(),
            i64::from(season),
            i64::from(episode),
            locale,
            fields.title.value(),
            fields.overview.value(),
            fields.air_date.value(),
            fields.runtime_minutes.value().map(|r| i64::from(*r)),
            fields.rating.value(),
            fields.still_url.value(),
            details,
            serde_json::to_string(origins)?,
        ],
        |r| r.get(0),
    )?;
    Ok(revision)
}
