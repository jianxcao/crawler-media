//! Derived browse summaries: `library_items`, `library_units`, `library_item_terms`.
//!
//! Every producer of file/metadata facts reduces the affected media inside its own
//! transaction, so a card can never observe half-written facts and a failed write can
//! never leave a summary that disagrees with the facts.
//!
//! Only published files are library content. A pending file row exists for management
//! and recovery views, but it must not create a card, count towards size, or become a
//! playback source.

use crate::metadata::locale::{LocaleFallback, select_locale};

use domain::{LibraryId, MediaId, MetadataFields};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::StoreError;

/// One published file's summary-relevant facts.
pub(crate) struct FileFact {
    file_id: String,
    presence: String,
    size_bytes: Option<i64>,
    resolution: Option<String>,
    hdr: bool,
    has_still: bool,
    poster: bool,
    backdrop: bool,
    added_ns: Option<i64>,
    pub(crate) revision: i64,
}

/// Adopted metadata for one Library's selected locale.
struct Adopted {
    title: Option<String>,
    pub(crate) year: Option<i64>,
    rating: Option<f64>,
    runtime: Option<i64>,
    pub(crate) language: Option<String>,
    release_date: Option<String>,
    pub(crate) revision: i64,
    genres: Vec<String>,
    countries: Vec<String>,
}

/// Display facts for one card. The Library's own valid work NFO wins over the adopted
/// catalogue row, which in turn wins over the Media's base identity.
pub(crate) struct Facts {
    pub(crate) title: String,
    pub(crate) year: Option<i64>,
    pub(crate) rating: f64,
    pub(crate) runtime: i64,
    pub(crate) language: Option<String>,
    pub(crate) release_date: String,
    pub(crate) genres: Vec<String>,
    pub(crate) countries: Vec<String>,
    pub(crate) adopted: bool,
    pub(crate) revision: i64,
}

/// File counts and unit facts derived from the published files of one media.
#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) file: i64,
    pub(crate) available: i64,
    pub(crate) missing: i64,
    pub(crate) unknown: i64,
    pub(crate) size_bytes: Option<i64>,
    pub(crate) has_still: bool,
    pub(crate) poster_ref: Option<String>,
    pub(crate) backdrop_ref: Option<String>,
    pub(crate) added_ns: Option<i64>,
    pub(crate) resolution_rank: i64,
    pub(crate) hdr: bool,
}

mod card;
mod ordering;

pub(crate) use card::*;
pub(crate) use ordering::*;

pub(crate) fn unit_counts(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
) -> Result<(i64, i64, Option<String>), StoreError> {
    let (file_count, available): (i64, i64) = tx.query_row(
        "SELECT COUNT(DISTINCT u.file_id),
                COUNT(DISTINCT CASE WHEN f.presence_state = 'present' THEN u.file_id END)
         FROM library_file_units u
         JOIN library_files f ON f.file_id = u.file_id
         WHERE f.library_id = ?1 AND f.media_id = ?2 AND u.season = ?3 AND u.episode = ?4
           AND f.publication_state = 'published'",
        params![lib, media, season, episode],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let preferred: Option<String> = tx
        .query_row(
            "SELECT u.file_id
             FROM library_file_units u
             JOIN library_files f ON f.file_id = u.file_id
             WHERE f.library_id = ?1 AND f.media_id = ?2 AND u.season = ?3 AND u.episode = ?4
               AND f.publication_state = 'published' AND +f.presence_state = 'present'
             ORDER BY f.file_revision DESC, f.file_id ASC
             LIMIT 1",
            params![lib, media, season, episode],
            |r| r.get(0),
        )
        .optional()?;
    Ok((file_count, available, preferred))
}

pub(crate) fn clear_summaries(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![lib, media],
    )?;
    tx.execute(
        "DELETE FROM library_units WHERE library_id = ?1 AND media_id = ?2",
        params![lib, media],
    )?;
    // NFO candidates follow their sidecars: the last published file is gone.
    tx.execute(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND source_version = 'nfo_v1'",
        params![lib, media],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert_item(
    tx: &Connection,
    lib: &str,
    media: &str,
    counts: &Counts,
    preferred: Option<&str>,
    distinct_seasons: i64,
    distinct_episodes: i64,
    facts: &Facts,
) -> Result<(), StoreError> {
    let (kind, identity_state) = media_identity(tx, media)?;
    let title = facts.title.clone();
    let year = facts.year;
    let rating = facts.rating;
    let runtime = facts.runtime;
    let release_date = facts.release_date.clone();
    let metadata_revision = facts.revision;
    let scraped = i64::from(facts.adopted);
    let language = facts.language.clone();
    let added_at_sort = counts.added_ns.unwrap_or(i64::MIN);
    let missing = i64::from(counts.missing > 0);
    let shuffle = format!("{:020}", shuffle_key(media));

    tx.execute(
        "INSERT INTO library_items
            (library_id, media_id, kind, title, title_sort, year, identity_state,
             metadata_revision, item_revision, release_date_sort, rating_sort, runtime_sort,
             added_at_sort, language, size_bytes, size_sort, resolution_rank, hdr,
             scraped, missing, stable_shuffle_key, file_count, available_file_count,
             missing_file_count, unknown_file_count, distinct_episode_count, distinct_season_count,
             preferred_file_id, has_still, poster_ref, backdrop_ref)
         VALUES
            (?1, ?2, ?3, ?4, ?4, ?5, ?6,
             ?7, 1, ?8, ?9, ?10,
             ?11, ?12, ?13, COALESCE(?13, 0), ?14, ?15,
             ?16, ?17, ?18, ?19, ?20,
             ?21, ?22, ?23, ?24,
             ?25, ?26, ?27, ?28)
         ON CONFLICT(library_id, media_id) DO UPDATE SET
             kind = excluded.kind,
             title = excluded.title,
             title_sort = excluded.title_sort,
             year = excluded.year,
             identity_state = excluded.identity_state,
             metadata_revision = excluded.metadata_revision,
             release_date_sort = excluded.release_date_sort,
             rating_sort = excluded.rating_sort,
             runtime_sort = excluded.runtime_sort,
             added_at_sort = excluded.added_at_sort,
             language = excluded.language,
             size_bytes = excluded.size_bytes,
             size_sort = excluded.size_sort,
             resolution_rank = excluded.resolution_rank,
             hdr = excluded.hdr,
             scraped = excluded.scraped,
             missing = excluded.missing,
             stable_shuffle_key = excluded.stable_shuffle_key,
             file_count = excluded.file_count,
             available_file_count = excluded.available_file_count,
             missing_file_count = excluded.missing_file_count,
             unknown_file_count = excluded.unknown_file_count,
             distinct_episode_count = excluded.distinct_episode_count,
             distinct_season_count = excluded.distinct_season_count,
             preferred_file_id = excluded.preferred_file_id,
             has_still = excluded.has_still,
             poster_ref = excluded.poster_ref,
             backdrop_ref = excluded.backdrop_ref,
             item_revision = library_items.item_revision + 1",
        params![
            lib,
            media,
            kind,
            title,
            year,
            identity_state,
            metadata_revision,
            release_date,
            rating,
            runtime,
            added_at_sort,
            language,
            counts.size_bytes,
            counts.resolution_rank,
            i64::from(counts.hdr),
            scraped,
            missing,
            shuffle,
            counts.file,
            counts.available,
            counts.missing,
            counts.unknown,
            distinct_episodes,
            distinct_seasons,
            preferred,
            i64::from(counts.has_still),
            counts.poster_ref.as_deref(),
            counts.backdrop_ref.as_deref(),
        ],
    )?;
    Ok(())
}

/// Kind and identity state of one Media, for the card summary.
///
/// This used to also build `library_items.aliases_json` with five per-source
/// lookups on every card reduce. The v4 round deleted that column (it had no
/// reader outside the tests), so the five queries went with it; the aliases a
/// client can see live on `media.aliases_display_json`.
pub(crate) fn media_identity(tx: &Connection, media: &str) -> Result<(String, String), StoreError> {
    tx.query_row(
        "SELECT kind, identity_state FROM media WHERE media_id = ?1",
        params![media],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| StoreError::Missing(format!("media {media}")))
}

/// Merge this Library's valid work NFO fields with the adopted catalogue row.
///
/// Precedence is the documented one: the Library's own NFO wins, the adopted row fills the
/// gaps, and the Media identity is the last resort. Card facts and terms both read this one
/// view so a card can never disagree with its filters.
pub(crate) fn facts_for(
    tx: &Connection,
    lib: &str,
    media: &str,
    preferred: Option<&str>,
) -> Result<Facts, StoreError> {
    let (media_title, media_year): (String, Option<i64>) = tx.query_row(
        "SELECT title, year FROM media WHERE media_id = ?1",
        params![media],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let adopted = adopted_for_library(tx, lib, media)?;
    let nfo = nfo_fields_for(tx, lib, media, preferred)?;

    let title = nfo
        .title
        .into_value()
        .or_else(|| adopted.as_ref().and_then(|a| a.title.clone()))
        .unwrap_or(media_title);
    let year = nfo
        .year
        .into_value()
        .map(i64::from)
        .or_else(|| adopted.as_ref().and_then(|a| a.year))
        .or(media_year);
    let rating = nfo
        .rating
        .into_value()
        .or_else(|| adopted.as_ref().and_then(|a| a.rating))
        .unwrap_or(-1.0);
    let runtime = nfo
        .runtime_minutes
        .into_value()
        .map(i64::from)
        .or_else(|| adopted.as_ref().and_then(|a| a.runtime))
        .unwrap_or(0);
    let language = nfo
        .language
        .into_value()
        .or_else(|| adopted.as_ref().and_then(|a| a.language.clone()));
    let release_date = nfo
        .release_date
        .into_value()
        .or_else(|| adopted.as_ref().and_then(|a| a.release_date.clone()))
        .filter(|value| !value.trim().is_empty())
        .or_else(|| year.map(|y| format!("{y:04}-01-01")))
        .unwrap_or_default();
    let genres = nfo
        .genres
        .into_value()
        .filter(|genres| !genres.is_empty())
        .map(|genres| genres.into_iter().map(|genre| genre.filter_key).collect())
        .unwrap_or_else(|| {
            adopted
                .as_ref()
                .map(|a| a.genres.clone())
                .unwrap_or_default()
        });
    let countries = nfo
        .countries
        .into_value()
        .filter(|codes| !codes.is_empty())
        .map(|codes| codes.into_iter().map(|code| code.to_uppercase()).collect())
        .unwrap_or_else(|| {
            adopted
                .as_ref()
                .map(|a| a.countries.clone())
                .unwrap_or_default()
        });
    Ok(Facts {
        title,
        year,
        rating,
        runtime,
        language,
        release_date,
        genres,
        countries,
        adopted: adopted.is_some(),
        revision: adopted.as_ref().map(|a| a.revision).unwrap_or(0),
    })
}

/// The Library's valid work-scope NFO fields for the locale chosen by the shared
/// selector (Library locale, else only the unbound bucket), preferred file first.
fn nfo_fields_for(
    tx: &Connection,
    lib: &str,
    media: &str,
    preferred: Option<&str>,
) -> Result<MetadataFields, StoreError> {
    let locale: Option<String> = tx
        .query_row(
            "SELECT COALESCE(NULLIF(TRIM(display_locale),''),(SELECT default_locale FROM library_query_config WHERE id=1),'') FROM libraries WHERE library_id = ?1",
            params![lib],
            |r| r.get(0),
        )
        .optional()?;
    let preferred_key: Option<String> = match preferred {
        Some(file_id) => tx
            .query_row(
                "SELECT path_key FROM library_files WHERE file_id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    let available = valid_work_input_locales(tx, lib, media)?;
    let chain: Vec<String> = locale.into_iter().collect();
    let Some(chosen) = select_locale(&available, &chain, LocaleFallback::UnboundInputOnly) else {
        return Ok(MetadataFields::default());
    };
    let fields_json: Option<String> = tx
        .query_row(
            "SELECT fields_json FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work'
               AND parse_state = 'valid' AND locale = ?3
             ORDER BY (path_key = norm_path_key(?4)) DESC, input_key
             LIMIT 1",
            params![lib, media, chosen, preferred_key],
            |r| r.get(0),
        )
        .optional()?;
    match fields_json {
        Some(fields_json) => Ok(serde_json::from_str(&fields_json)?),
        None => Ok(MetadataFields::default()),
    }
}

fn adopted_for_library(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Option<Adopted>, StoreError> {
    let locale: Option<String> = tx
        .query_row(
            "SELECT COALESCE(NULLIF(TRIM(display_locale),''),(SELECT default_locale FROM library_query_config WHERE id=1),'') FROM libraries WHERE library_id = ?1",
            params![lib],
            |r| r.get(0),
        )
        .optional()?;
    const COLS: &str = "SELECT title, year, rating, runtime_minutes, language, release_date,
                genres_json, countries_json, revision
         FROM media_metadata WHERE media_id = ?1";
    let read = |r: &rusqlite::Row<'_>| -> rusqlite::Result<Adopted> {
        let genres_raw: String = r.get(6)?;
        let countries_raw: String = r.get(7)?;
        Ok(Adopted {
            title: r.get(0)?,
            year: r.get(1)?,
            rating: r.get(2)?,
            runtime: r.get(3)?,
            language: r.get(4)?,
            release_date: r.get(5)?,
            revision: r.get(8)?,
            genres: genre_keys(&genres_raw),
            countries: country_codes(&countries_raw),
        })
    };
    let mut stmt = tx.prepare("SELECT locale FROM media_metadata WHERE media_id = ?1")?;
    let available = stmt
        .query_map(params![media], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let chain: Vec<String> = locale.into_iter().collect();
    let Some(chosen) = select_locale(&available, &chain, LocaleFallback::AdoptedFirst) else {
        return Ok(None);
    };
    Ok(tx
        .query_row(
            &format!("{COLS} AND locale = ?2"),
            params![media, chosen],
            read,
        )
        .optional()?)
}

fn valid_work_input_locales(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT locale FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work' AND parse_state = 'valid'",
    )?;
    let rows = stmt.query_map(params![lib, media], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn genre_keys(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<domain::MetadataGenre>>(raw)
        .map(|genres| genres.into_iter().map(|genre| genre.filter_key).collect())
        .unwrap_or_default()
}

fn country_codes(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw)
        .map(|codes| codes.into_iter().map(|code| code.to_uppercase()).collect())
        .unwrap_or_default()
}

pub(crate) fn insert_term(
    tx: &Connection,
    lib: &str,
    media: &str,
    kind: &str,
    value: &str,
) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    tx.execute(
        "INSERT OR IGNORE INTO library_item_terms (library_id, term_kind, term_value, media_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![lib, kind, value, media],
    )?;
    Ok(())
}
