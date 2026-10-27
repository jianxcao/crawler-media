//! Adopted metadata half of a Media merge: per-locale `media_metadata` rows (survivor
//! wins, loser fills only what is missing) and the shared `media_metadata_overrides`
//! fields. Drives both halves in [`merge_metadata`].

use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::snapshots::{
    json_object, loser_metadata_keys, merge_source_snapshots, recompose_selected,
};
use crate::StoreError;

/// One adopted-metadata row. Missing scalar columns and fields absent from
/// `field_origins_json` are the only values the loser may fill.
struct AdoptedRow {
    locale: String,
    title: String,
    original_title: Option<String>,
    year: Option<i64>,
    overview: Option<String>,
    release_date: Option<String>,
    rating: Option<f64>,
    runtime_minutes: Option<i64>,
    language: Option<String>,
    genres_json: String,
    countries_json: String,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    details_json: String,
    field_origins_json: String,
    revision: i64,
}

const ADOPTED_COLUMNS: &str = "locale, title, original_title, year, overview, release_date, \
     rating, runtime_minutes, language, genres_json, countries_json, poster_url, backdrop_url, \
     details_json, field_origins_json, revision";

fn load_adopted(
    tx: &Connection,
    media: MediaId,
    locale: &str,
) -> Result<Option<AdoptedRow>, StoreError> {
    let sql =
        format!("SELECT {ADOPTED_COLUMNS} FROM media_metadata WHERE media_id = ?1 AND locale = ?2");
    tx.query_row(&sql, params![media.to_string(), locale], |r| {
        Ok(AdoptedRow {
            locale: r.get(0)?,
            title: r.get(1)?,
            original_title: r.get(2)?,
            year: r.get(3)?,
            overview: r.get(4)?,
            release_date: r.get(5)?,
            rating: r.get(6)?,
            runtime_minutes: r.get(7)?,
            language: r.get(8)?,
            genres_json: r.get(9)?,
            countries_json: r.get(10)?,
            poster_url: r.get(11)?,
            backdrop_url: r.get(12)?,
            details_json: r.get(13)?,
            field_origins_json: r.get(14)?,
            revision: r.get(15)?,
        })
    })
    .optional()
    .map_err(Into::into)
}


/// Survivor wins; the loser only fills fields the survivor is missing. Explicit
/// empty values are values and stay untouched. Never merges across locales.
fn merge_adopted(survivor: AdoptedRow, loser: AdoptedRow) -> Result<AdoptedRow, StoreError> {
    let survivor_origins = json_object(&survivor.field_origins_json)?;
    let loser_origins = json_object(&loser.field_origins_json)?;
    let mut origins = survivor_origins.clone();
    for (key, value) in &loser_origins {
        origins.entry(key.clone()).or_insert_with(|| value.clone());
    }

    let mut details = json_object(&survivor.details_json)?;
    let mut fields = details
        .get("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let loser_details = json_object(&loser.details_json)?;
    if let Some(loser_fields) = loser_details.get("fields").and_then(Value::as_object) {
        for (key, value) in loser_fields {
            fields.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    details.insert("fields".to_string(), Value::Object(fields));

    let array_field = |name: &str, survivor_value: &str, loser_value: &str| {
        if survivor_origins.contains_key(name) || !loser_origins.contains_key(name) {
            survivor_value.to_string()
        } else {
            loser_value.to_string()
        }
    };

    Ok(AdoptedRow {
        locale: survivor.locale,
        title: survivor.title,
        original_title: survivor.original_title.or(loser.original_title),
        year: survivor.year.or(loser.year),
        overview: survivor.overview.or(loser.overview),
        release_date: survivor.release_date.or(loser.release_date),
        rating: survivor.rating.or(loser.rating),
        runtime_minutes: survivor.runtime_minutes.or(loser.runtime_minutes),
        language: survivor.language.or(loser.language),
        genres_json: array_field("genres", &survivor.genres_json, &loser.genres_json),
        countries_json: array_field("countries", &survivor.countries_json, &loser.countries_json),
        poster_url: survivor.poster_url.or(loser.poster_url),
        backdrop_url: survivor.backdrop_url.or(loser.backdrop_url),
        details_json: serde_json::to_string(&Value::Object(details))?,
        field_origins_json: serde_json::to_string(&Value::Object(origins))?,
        revision: survivor.revision.max(loser.revision) + 1,
    })
}

fn store_adopted(tx: &Connection, media: MediaId, row: &AdoptedRow) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE media_metadata SET title = ?2, original_title = ?3, year = ?4, overview = ?5,
             release_date = ?6, rating = ?7, runtime_minutes = ?8, language = ?9,
             genres_json = ?10, countries_json = ?11, poster_url = ?12, backdrop_url = ?13,
             details_json = ?14, field_origins_json = ?15, revision = ?16
         WHERE media_id = ?1 AND locale = ?17",
        params![
            media.to_string(),
            row.title,
            row.original_title,
            row.year,
            row.overview,
            row.release_date,
            row.rating,
            row.runtime_minutes,
            row.language,
            row.genres_json,
            row.countries_json,
            row.poster_url,
            row.backdrop_url,
            row.details_json,
            row.field_origins_json,
            row.revision,
            row.locale,
        ],
    )?;
    Ok(())
}

pub(in crate::media_operations) fn merge_metadata(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let (survivor_str, loser_str) = (survivor.to_string(), loser.to_string());
    let affected = loser_metadata_keys(tx, &loser_str)?;
    merge_source_snapshots(tx, &survivor_str, &loser_str)?;
    // Shared override keys: preflight refused conflicting fields, so the loser's fields
    // the survivor lacks are folded into the survivor's row before the loser row goes.
    merge_shared_overrides(tx, &survivor_str, &loser_str)?;
    tx.execute(
        "DELETE FROM media_metadata_overrides WHERE media_id = ?2 AND EXISTS (
             SELECT 1 FROM media_metadata_overrides o
             WHERE o.media_id = ?1
               AND o.entity_scope = media_metadata_overrides.entity_scope
               AND o.season = media_metadata_overrides.season
               AND o.episode = media_metadata_overrides.episode
               AND o.locale = media_metadata_overrides.locale)",
        params![survivor_str, loser_str],
    )?;
    tx.execute(
        "UPDATE media_metadata_overrides SET media_id = ?1 WHERE media_id = ?2",
        params![survivor_str, loser_str],
    )?;

    // Episode adopted rows are keyed by their coordinate, so the survivor's row
    // wins on a shared key and the loser's unique rows move. Leaving them behind
    // would strand metadata on the retired tombstone. (`media_seasons` had the
    // same shape until the v4 round deleted the table: nothing read it.)
    tx.execute(
        "DELETE FROM media_episodes WHERE media_id = ?2 AND EXISTS (
             SELECT 1 FROM media_episodes e
             WHERE e.media_id = ?1
               AND e.season = media_episodes.season
               AND e.episode = media_episodes.episode
               AND e.locale = media_episodes.locale)",
        params![survivor_str, loser_str],
    )?;
    tx.execute(
        "UPDATE media_episodes SET media_id = ?1 WHERE media_id = ?2",
        params![survivor_str, loser_str],
    )?;

    let locales: Vec<String> = {
        let mut stmt = tx.prepare("SELECT locale FROM media_metadata WHERE media_id = ?1")?;
        stmt.query_map(params![loser_str], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for locale in locales {
        let Some(loser_row) = load_adopted(tx, loser, &locale)? else {
            continue;
        };
        match load_adopted(tx, survivor, &locale)? {
            None => {
                tx.execute(
                    "UPDATE media_metadata SET media_id = ?1 WHERE media_id = ?2 AND locale = ?3",
                    params![survivor_str, loser_str, locale],
                )?;
            }
            Some(survivor_row) => {
                let merged = merge_adopted(survivor_row, loser_row)?;
                store_adopted(tx, survivor, &merged)?;
                tx.execute(
                    "DELETE FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
                    params![loser_str, locale],
                )?;
            }
        }
    }
    recompose_selected(tx, survivor, &affected)
}

fn merge_shared_overrides(tx: &Connection, survivor: &str, loser: &str) -> Result<(), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT s.entity_scope, s.season, s.episode, s.locale, s.fields_json, l.fields_json
           FROM media_metadata_overrides s
           JOIN media_metadata_overrides l
             ON l.entity_scope = s.entity_scope AND l.season = s.season
            AND l.episode = s.episode AND l.locale = s.locale
          WHERE s.media_id = ?1 AND l.media_id = ?2",
    )?;
    type Row = (String, i64, i64, String, String, String);
    let rows: Vec<Row> = stmt
        .query_map(params![survivor, loser], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (scope, season, episode, locale, survivor_json, loser_json) in rows {
        let mut merged: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&survivor_json)?;
        let loser_fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&loser_json)?;
        let before = merged.len();
        for (key, value) in loser_fields {
            merged.entry(key).or_insert(value);
        }
        if merged.len() == before {
            continue;
        }
        tx.execute(
            "UPDATE media_metadata_overrides SET fields_json = ?6, revision = revision + 1
             WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND locale = ?5",
            params![survivor, scope, season, episode, locale, serde_json::Value::Object(merged).to_string()],
        )?;
    }
    Ok(())
}
