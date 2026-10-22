//! Fact merges for a canonical Media merge (§4.6): adopted metadata, user
//! playback facts and collection membership. Every function runs inside the
//! caller's `library.db` transaction and is deterministic from the surviving
//! rows, so re-running a phase after a crash cannot duplicate or double-count.

use domain::MediaId;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::{Map, Value};

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

fn json_object(raw: &str) -> Result<Map<String, Value>, StoreError> {
    match serde_json::from_str::<Value>(raw)? {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
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

pub(super) fn merge_metadata(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let (survivor_str, loser_str) = (survivor.to_string(), loser.to_string());
    // Source snapshots: for the *same* provider entity the newer fetch wins as a whole
    // snapshot (a tie keeps the survivor's). Otherwise the survivor's row wins on a shared
    // key. A conflicting snapshot is superseded, never spliced field-by-field.
    tx.execute(
        "DELETE FROM media_metadata_sources WHERE media_id = ?1 AND EXISTS (
             SELECT 1 FROM media_metadata_sources l
             WHERE l.media_id = ?2
               AND l.entity_scope = media_metadata_sources.entity_scope
               AND l.season = media_metadata_sources.season
               AND l.episode = media_metadata_sources.episode
               AND l.source = media_metadata_sources.source
               AND l.locale = media_metadata_sources.locale
               AND l.provider_entity_id = media_metadata_sources.provider_entity_id
               AND l.fetched_at > media_metadata_sources.fetched_at)",
        params![survivor_str, loser_str],
    )?;
    tx.execute(
        "DELETE FROM media_metadata_sources WHERE media_id = ?2 AND EXISTS (
             SELECT 1 FROM media_metadata_sources s
             WHERE s.media_id = ?1
               AND s.entity_scope = media_metadata_sources.entity_scope
               AND s.season = media_metadata_sources.season
               AND s.episode = media_metadata_sources.episode
               AND s.source = media_metadata_sources.source
               AND s.locale = media_metadata_sources.locale)",
        params![survivor_str, loser_str],
    )?;
    tx.execute(
        "UPDATE media_metadata_sources SET media_id = ?1 WHERE media_id = ?2",
        params![survivor_str, loser_str],
    )?;
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

    // Season/episode adopted rows are keyed by their coordinate, so the
    // survivor's row wins on a shared key and the loser's unique rows move.
    // Leaving them behind would strand metadata on the retired tombstone.
    tx.execute(
        "DELETE FROM media_seasons WHERE media_id = ?2 AND EXISTS (
             SELECT 1 FROM media_seasons s
             WHERE s.media_id = ?1
               AND s.season = media_seasons.season
               AND s.locale = media_seasons.locale)",
        params![survivor_str, loser_str],
    )?;
    tx.execute(
        "UPDATE media_seasons SET media_id = ?1 WHERE media_id = ?2",
        params![survivor_str, loser_str],
    )?;
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
    Ok(())
}

/// One `playback_units` row. The resume columns are moved as a package.
struct PlaybackRow {
    user_id: String,
    season: i32,
    episode: i32,
    played: i64,
    has_played_assertion: i64,
    played_changed_at_ms: Option<i64>,
    favorite: i64,
    favorite_at: i64,
    play_count: i64,
    watch_activity_at: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
    resume_file_id: Option<String>,
    resume_file_revision: Option<i64>,
    resume_source_version: Option<String>,
    timeline_mode: Option<String>,
    tracks_version: Option<i64>,
    audio_track: Option<String>,
    subtitle_track: Option<String>,
}

const PLAYBACK_COLUMNS: &str = "user_id, season, episode, played, has_played_assertion, \
     played_changed_at_ms, favorite, favorite_at, play_count, watch_activity_at, position_ms, \
     duration_ms, resume_file_id, resume_file_revision, resume_source_version, timeline_mode, \
     tracks_version, audio_track, subtitle_track";

fn load_playback(
    tx: &Connection,
    media: MediaId,
    user_id: &str,
    season: i32,
    episode: i32,
) -> Result<Option<PlaybackRow>, StoreError> {
    let sql = format!(
        "SELECT {PLAYBACK_COLUMNS} FROM playback_units
         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4"
    );
    tx.query_row(
        &sql,
        params![user_id, media.to_string(), season, episode],
        |r| {
            Ok(PlaybackRow {
                user_id: r.get(0)?,
                season: r.get(1)?,
                episode: r.get(2)?,
                played: r.get(3)?,
                has_played_assertion: r.get(4)?,
                played_changed_at_ms: r.get(5)?,
                favorite: r.get(6)?,
                favorite_at: r.get(7)?,
                play_count: r.get(8)?,
                watch_activity_at: r.get(9)?,
                position_ms: r.get(10)?,
                duration_ms: r.get(11)?,
                resume_file_id: r.get(12)?,
                resume_file_revision: r.get(13)?,
                resume_source_version: r.get(14)?,
                timeline_mode: r.get(15)?,
                tracks_version: r.get(16)?,
                audio_track: r.get(17)?,
                subtitle_track: r.get(18)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Merge one user/unit pair. Favourite is OR, play count is max (never summed),
/// the played assertion is a tri-state over rows that actually asserted it, and
/// the resume package comes whole from the later `watch_activity_at` row.
fn merge_playback_rows(survivor: PlaybackRow, loser: PlaybackRow) -> PlaybackRow {
    let has_assertion = survivor.has_played_assertion == 1 || loser.has_played_assertion == 1;
    let (played, played_changed_at_ms) = if has_assertion {
        (
            i64::from(survivor.played == 1 || loser.played == 1),
            survivor
                .played_changed_at_ms
                .into_iter()
                .chain(loser.played_changed_at_ms)
                .max(),
        )
    } else {
        (0, None)
    };
    let favorite = i64::from(survivor.favorite == 1 || loser.favorite == 1);
    let favorite_at = survivor.favorite_at.max(loser.favorite_at);
    let play_count = survivor.play_count.max(loser.play_count);
    let watch_activity_at = survivor.watch_activity_at.max(loser.watch_activity_at);
    let resume = if loser.watch_activity_at > survivor.watch_activity_at {
        loser
    } else {
        survivor
    };
    PlaybackRow {
        user_id: resume.user_id.clone(),
        season: resume.season,
        episode: resume.episode,
        played,
        has_played_assertion: i64::from(has_assertion),
        played_changed_at_ms,
        favorite,
        favorite_at,
        play_count,
        watch_activity_at,
        position_ms: resume.position_ms,
        duration_ms: resume.duration_ms,
        resume_file_id: resume.resume_file_id,
        resume_file_revision: resume.resume_file_revision,
        resume_source_version: resume.resume_source_version,
        timeline_mode: resume.timeline_mode,
        tracks_version: resume.tracks_version,
        audio_track: resume.audio_track,
        subtitle_track: resume.subtitle_track,
    }
}

fn store_playback(tx: &Connection, media: MediaId, row: &PlaybackRow) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE playback_units SET played = ?5, has_played_assertion = ?6,
             played_changed_at_ms = ?7, favorite = ?8, favorite_at = ?9, play_count = ?10,
             watch_activity_at = ?11, position_ms = ?12, duration_ms = ?13,
             resume_file_id = ?14, resume_file_revision = ?15, resume_source_version = ?16,
             timeline_mode = ?17, tracks_version = ?18, audio_track = ?19, subtitle_track = ?20
         WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
        params![
            row.user_id,
            media.to_string(),
            row.season,
            row.episode,
            row.played,
            row.has_played_assertion,
            row.played_changed_at_ms,
            row.favorite,
            row.favorite_at,
            row.play_count,
            row.watch_activity_at,
            row.position_ms,
            row.duration_ms,
            row.resume_file_id,
            row.resume_file_revision,
            row.resume_source_version,
            row.timeline_mode,
            row.tracks_version,
            row.audio_track,
            row.subtitle_track,
        ],
    )?;
    Ok(())
}

pub(super) fn merge_playback_units(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let keys: Vec<(String, i32, i32)> = {
        let mut stmt =
            tx.prepare("SELECT user_id, season, episode FROM playback_units WHERE media_id = ?1")?;
        stmt.query_map(params![loser.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (user_id, season, episode) in keys {
        let loser_row = load_playback(tx, loser, &user_id, season, episode)?.ok_or_else(|| {
            StoreError::Invalid(format!(
                "playback row for {user_id} ({season},{episode}) vanished during merge"
            ))
        })?;
        match load_playback(tx, survivor, &user_id, season, episode)? {
            None => {
                tx.execute(
                    "UPDATE playback_units SET media_id = ?1
                     WHERE user_id = ?2 AND media_id = ?3 AND season = ?4 AND episode = ?5",
                    params![
                        survivor.to_string(),
                        user_id,
                        loser.to_string(),
                        season,
                        episode
                    ],
                )?;
            }
            Some(survivor_row) => {
                let merged = merge_playback_rows(survivor_row, loser_row);
                store_playback(tx, survivor, &merged)?;
                tx.execute(
                    "DELETE FROM playback_units
                     WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                    params![user_id, loser.to_string(), season, episode],
                )?;
            }
        }
    }
    Ok(())
}

/// Media-scoped watch facts that no file move can carry: timeline markers, the
/// fingerprint season models built from the films' samples, and the season
/// comparison history. They follow the identity, so the retired loser keeps none
/// of them; for a key the survivor already holds, the survivor's own row wins.
pub(super) fn merge_watch_facts(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    move_scoped_rows(tx, "media_markers", &["season", "episode"], survivor, loser)?;
    move_scoped_rows(
        tx,
        "fingerprint_season_models",
        &["season", "kind"],
        survivor,
        loser,
    )?;
    move_scoped_rows(
        tx,
        "probe_season_comparison_history",
        &["season"],
        survivor,
        loser,
    )?;
    Ok(())
}

/// Re-point the loser's rows of one media-scoped table onto the survivor. `key`
/// names the table's remaining key columns: for a key the survivor already holds
/// its own row wins and the loser's duplicate is dropped, so the loser is left
/// with no row at all.
fn move_scoped_rows(
    tx: &Connection,
    table: &str,
    key: &[&str],
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let rows: Vec<Vec<SqlValue>> = {
        let sql = format!("SELECT {} FROM {table} WHERE media_id = ?1", key.join(", "));
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt
            .query_map(params![loser.to_string()], |r| {
                (0..key.len()).map(|i| r.get::<_, SqlValue>(i)).collect()
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let predicate = |offset: usize| {
        key.iter()
            .enumerate()
            .map(|(i, column)| format!("{column} IS ?{}", i + offset))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    for row in rows {
        let mut owned = vec![SqlValue::Text(survivor.to_string())];
        owned.extend(row.iter().cloned());
        let survivor_has_it: Option<i64> = tx
            .query_row(
                &format!(
                    "SELECT 1 FROM {table} WHERE media_id = ?1 AND {}",
                    predicate(2)
                ),
                params_from_iter(owned),
                |r| r.get(0),
            )
            .optional()?;

        let mut gone = vec![
            SqlValue::Text(survivor.to_string()),
            SqlValue::Text(loser.to_string()),
        ];
        gone.extend(row.iter().cloned());
        if survivor_has_it.is_some() {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE media_id = ?2 AND {}",
                    predicate(3)
                ),
                params_from_iter(gone),
            )?;
            continue;
        }
        tx.execute(
            &format!(
                "UPDATE {table} SET media_id = ?1 WHERE media_id = ?2 AND {}",
                predicate(3)
            ),
            params_from_iter(gone),
        )?;
    }
    Ok(())
}

/// Collection membership: keep one survivor row per collection at the earliest
/// sort position. Other members are untouched.
pub(super) fn merge_collection_items(
    tx: &Connection,
    survivor: MediaId,
    loser: MediaId,
) -> Result<(), StoreError> {
    let rows: Vec<(String, i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT collection_id, sort_order, added_at FROM collection_items
             WHERE media_id = ?1",
        )?;
        stmt.query_map(params![loser.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (collection_id, sort_order, added_at) in rows {
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT sort_order, added_at FROM collection_items
                 WHERE collection_id = ?1 AND media_id = ?2",
                params![collection_id, survivor.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                tx.execute(
                    "UPDATE collection_items SET media_id = ?1
                     WHERE collection_id = ?2 AND media_id = ?3",
                    params![survivor.to_string(), collection_id, loser.to_string()],
                )?;
            }
            Some((survivor_sort, survivor_added)) => {
                let sort_order = survivor_sort.min(sort_order);
                let added_at = if survivor_added <= added_at {
                    survivor_added
                } else {
                    added_at
                };
                tx.execute(
                    "UPDATE collection_items SET sort_order = ?1, added_at = ?2
                     WHERE collection_id = ?3 AND media_id = ?4",
                    params![sort_order, added_at, collection_id, survivor.to_string()],
                )?;
                tx.execute(
                    "DELETE FROM collection_items WHERE collection_id = ?1 AND media_id = ?2",
                    params![collection_id, loser.to_string()],
                )?;
            }
        }
    }
    Ok(())
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
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (scope, season, episode, locale, survivor_json, loser_json) in rows {
        let mut merged: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&survivor_json)?;
        let loser_fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&loser_json)?;
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
