//! Derived browse summaries: `library_items`, `library_units`, `library_item_terms`.
//!
//! Every producer of file/metadata facts reduces the affected media inside its own
//! transaction, so a card can never observe half-written facts and a failed write can
//! never leave a summary that disagrees with the facts.
//!
//! Only published files are library content. A pending file row exists for management
//! and recovery views, but it must not create a card, count towards size, or become a
//! playback source.

use std::collections::HashSet;

use domain::{LibraryId, MediaId, MetadataFields};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::super::StoreError;

/// One published file's summary-relevant facts.
struct FileFact {
    file_id: String,
    presence: String,
    size_bytes: Option<i64>,
    resolution: Option<String>,
    hdr: bool,
    has_still: bool,
    poster: bool,
    backdrop: bool,
    added_ns: Option<i64>,
    revision: i64,
}

/// Adopted metadata for one library's display locale (falls back to any locale row).
struct Adopted {
    title: Option<String>,
    year: Option<i64>,
    rating: Option<f64>,
    runtime: Option<i64>,
    language: Option<String>,
    release_date: Option<String>,
    revision: i64,
    genres: Vec<String>,
    countries: Vec<String>,
}

/// Display facts for one card. The Library's own valid work NFO wins over the adopted
/// catalogue row, which in turn wins over the Media's base identity.
pub(super) struct Facts {
    title: String,
    year: Option<i64>,
    rating: f64,
    runtime: i64,
    language: Option<String>,
    release_date: String,
    pub(super) genres: Vec<String>,
    pub(super) countries: Vec<String>,
    adopted: bool,
    revision: i64,
}

/// File counts and unit facts derived from the published files of one media.
#[derive(Default)]
pub(super) struct Counts {
    pub(super) file: i64,
    pub(super) available: i64,
    pub(super) missing: i64,
    pub(super) unknown: i64,
    pub(super) size_bytes: Option<i64>,
    pub(super) has_still: bool,
    pub(super) poster_ref: Option<String>,
    pub(super) backdrop_ref: Option<String>,
    pub(super) added_ns: Option<i64>,
    pub(super) resolution_rank: i64,
    pub(super) hdr: bool,
}

/// Recompute every summary owned by `(library_id, media_id)` in the caller's transaction.
pub(crate) fn reduce_library_media(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<(), StoreError> {
    let lib = library_id.to_string();
    let media = media_id.to_string();
    let files = published_files(tx, &lib, &media)?;
    if files.is_empty() {
        clear_summaries(tx, &lib, &media)?;
        return Ok(());
    }
    let counts = counts_of(&files);
    let preferred = preferred_file(&files);
    prune_nfo_inputs(tx, &lib, &media)?;
    let (distinct_seasons, distinct_episodes) = rebuild_units(tx, &lib, &media)?;
    let facts = facts_for(tx, &lib, &media, preferred.as_deref())?;
    upsert_item(
        tx,
        &lib,
        &media,
        &counts,
        preferred.as_deref(),
        distinct_seasons,
        distinct_episodes,
        &facts,
    )?;
    sync_terms(tx, &lib, &media, &facts)?;
    refresh_affected_user_summaries(tx, library_id, media_id)?;
    Ok(())
}

/// Reduce every library that owns files of `media_id`. Returns the number reduced.
pub(crate) fn reduce_media_everywhere(
    tx: &Connection,
    media_id: MediaId,
) -> Result<usize, StoreError> {
    let mut stmt =
        tx.prepare("SELECT DISTINCT library_id FROM library_files WHERE media_id = ?1")?;
    let libs = stmt
        .query_map(params![media_id.to_string()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for lib in &libs {
        let library_id = lib.parse::<LibraryId>()?;
        reduce_library_media(tx, library_id, media_id)?;
    }
    Ok(libs.len())
}
fn refresh_affected_user_summaries(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<(), StoreError> {
    let users: Vec<String> = {
        let mut stmt = tx.prepare("SELECT DISTINCT user_id FROM playback_units WHERE media_id = ?1 ORDER BY user_id")?;
        let rows = stmt.query_map([media_id.to_string()], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if users.is_empty() {
        return Ok(());
    }
    let is_tv: bool = tx
        .query_row(
            "SELECT kind = 'tv' FROM media WHERE media_id = ?1",
            [media_id.to_string()],
            |r| r.get(0),
        )
        .unwrap_or(false);
    let lib_str = library_id.to_string();
    for user_str in users {
        let Ok(user_id) = user_str.parse::<domain::UserId>() else {
            continue;
        };
        let facts = crate::Store::summary_facts(tx, user_id, media_id)?;
        crate::Store::write_library_summary(tx, user_id, media_id, &lib_str, is_tv, &facts)?;
    }
    Ok(())
}

fn published_files(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Vec<FileFact>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT file_id, presence_state, size_bytes, resolution, hdr, has_still,
                has_poster, has_backdrop, COALESCE(created_ns, mtime_ns), file_revision
         FROM library_files
         WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'",
    )?;
    let rows = stmt
        .query_map(params![lib, media], |r| {
            let hdr: Option<String> = r.get(4)?;
            Ok(FileFact {
                file_id: r.get(0)?,
                presence: r.get(1)?,
                size_bytes: r.get(2)?,
                resolution: r.get(3)?,
                hdr: hdr.is_some_and(|value| !value.trim().is_empty()),
                has_still: r.get::<_, i64>(5)? != 0,
                poster: r.get::<_, i64>(6)? != 0,
                backdrop: r.get::<_, i64>(7)? != 0,
                added_ns: r.get(8)?,
                revision: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn counts_of(files: &[FileFact]) -> Counts {
    let mut counts = Counts {
        file: files.len() as i64,
        resolution_rank: 1,
        ..Counts::default()
    };
    let mut known_size = 0i64;
    let mut any_known_size = false;
    for file in files {
        match file.presence.as_str() {
            "present" => counts.available += 1,
            "missing" => counts.missing += 1,
            _ => counts.unknown += 1,
        }
        if let Some(size) = file.size_bytes {
            known_size += size;
            any_known_size = true;
        }
        counts.has_still |= file.has_still;
        counts.hdr |= file.hdr;
        counts.resolution_rank = counts
            .resolution_rank
            .max(resolution_rank(file.resolution.as_deref().unwrap_or("")));
        counts.added_ns = match (counts.added_ns, file.added_ns) {
            (Some(current), Some(candidate)) => Some(current.min(candidate)),
            (None, candidate) => candidate,
            (current, None) => current,
        };
    }
    counts.poster_ref = best_art_file(files, |file| file.poster);
    counts.backdrop_ref = best_art_file(files, |file| file.backdrop);
    counts.size_bytes = any_known_size.then_some(known_size);
    counts
}

/// Best available file: published + present, newest revision, stable tie-break.
fn best_art_file(files: &[FileFact], has_art: impl Fn(&FileFact) -> bool) -> Option<String> {
    files
        .iter()
        .filter(|file| has_art(file))
        .max_by(|a, b| {
            (a.presence == "present")
                .cmp(&(b.presence == "present"))
                .then_with(|| a.revision.cmp(&b.revision))
                .then_with(|| b.file_id.cmp(&a.file_id))
        })
        .map(|file| file.file_id.clone())
}

fn preferred_file(files: &[FileFact]) -> Option<String> {
    files
        .iter()
        .filter(|file| file.presence == "present")
        .max_by(|a, b| {
            a.revision
                .cmp(&b.revision)
                .then_with(|| b.file_id.cmp(&a.file_id))
        })
        .map(|file| file.file_id.clone())
}

/// Rebuild `library_units` from published coverage. Returns distinct season/episode counts.
fn rebuild_units(tx: &Connection, lib: &str, media: &str) -> Result<(i64, i64), StoreError> {
    tx.execute(
        "DELETE FROM library_units
         WHERE library_id = ?1 AND media_id = ?2
           AND (season, episode) NOT IN (
               SELECT u.season, u.episode
               FROM library_file_units u
               JOIN library_files f ON f.file_id = u.file_id
               WHERE f.library_id = ?1 AND f.media_id = ?2
                 AND f.publication_state = 'published')",
        params![lib, media],
    )?;

    let groups = coverage_units(tx, lib, media)?;
    let mut seasons = HashSet::new();
    let mut episodes = HashSet::new();
    for (season, episode) in groups {
        let (file_count, available, preferred) =
            unit_counts(tx, lib, media, season, episode)?;
        if season >= 0 {
            seasons.insert(season);
        }
        if season >= 0 && episode >= 0 {
            episodes.insert((season, episode));
        }
        tx.execute(
            "INSERT INTO library_units
                (library_id, media_id, season, episode, file_count, available_file_count,
                 preferred_file_id, quality_json, unit_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', 1)
             ON CONFLICT(library_id, media_id, season, episode) DO UPDATE SET
                 file_count = excluded.file_count,
                 available_file_count = excluded.available_file_count,
                 preferred_file_id = excluded.preferred_file_id,
                 unit_revision = library_units.unit_revision + 1",
            params![lib, media, season, episode, file_count, available, preferred],
        )?;
    }
    Ok((seasons.len() as i64, episodes.len() as i64))
}

fn coverage_units(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Vec<(i32, i32)>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT u.season, u.episode
         FROM library_file_units u
         JOIN library_files f ON f.file_id = u.file_id
         WHERE f.library_id = ?1 AND f.media_id = ?2 AND f.publication_state = 'published'
         ORDER BY u.season, u.episode",
    )?;
    let rows = stmt
        .query_map(params![lib, media], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn unit_counts(
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
               AND f.publication_state = 'published' AND f.presence_state = 'present'
             ORDER BY f.file_revision DESC, f.file_id ASC
             LIMIT 1",
            params![lib, media, season, episode],
            |r| r.get(0),
        )
        .optional()?;
    Ok((file_count, available, preferred))
}

pub(super) fn clear_summaries(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
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

/// Drop NFO inputs whose sidecar no longer belongs to a published file of this card.
fn prune_nfo_inputs(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND source_version = 'nfo_v1'
           AND input_key NOT IN (
               SELECT path_key FROM library_files
               WHERE library_id = ?1 AND media_id = ?2
                 AND publication_state = 'published')",
        params![lib, media],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_item(
    tx: &Connection,
    lib: &str,
    media: &str,
    counts: &Counts,
    preferred: Option<&str>,
    distinct_seasons: i64,
    distinct_episodes: i64,
    facts: &Facts,
) -> Result<(), StoreError> {
    let (kind, identity_state, aliases) = media_identity(tx, media)?;
    let title = facts.title.clone();
    let year = facts.year;
    let rating = facts.rating;
    let runtime = facts.runtime;
    let release_date = facts.release_date.clone();
    let metadata_revision = facts.revision;
    let scraped = i64::from(facts.adopted);
    let has_nfo = has_valid_work_nfo(tx, lib, media)?;
    let language = facts.language.clone();
    let added_at_sort = counts.added_ns.unwrap_or(i64::MIN);
    let missing = i64::from(counts.missing > 0);
    let shuffle = format!("{:020}", shuffle_key(media));

    tx.execute(
        "INSERT INTO library_items
            (library_id, media_id, kind, title, title_sort, year, identity_state, aliases_json,
             metadata_revision, item_revision, release_date_sort, rating_sort, runtime_sort,
             added_at_ns, added_at_sort, language, size_bytes, size_sort, resolution_rank, hdr,
             scraped, has_nfo, missing, stable_shuffle_key, file_count, available_file_count,
             missing_file_count, unknown_file_count, distinct_episode_count, distinct_season_count,
             preferred_file_id, has_still, poster_ref, backdrop_ref)
         VALUES
            (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7,
             ?8, 1, ?9, ?10, ?11,
             ?12, ?13, ?14, ?15, COALESCE(?15, 0), ?16, ?17,
             ?18, ?19, ?20, ?21, ?22, ?23,
             ?24, ?25, ?26, ?27,
             ?28, ?29, ?30, ?31)
         ON CONFLICT(library_id, media_id) DO UPDATE SET
             kind = excluded.kind,
             title = excluded.title,
             title_sort = excluded.title_sort,
             year = excluded.year,
             identity_state = excluded.identity_state,
             aliases_json = excluded.aliases_json,
             metadata_revision = excluded.metadata_revision,
             release_date_sort = excluded.release_date_sort,
             rating_sort = excluded.rating_sort,
             runtime_sort = excluded.runtime_sort,
             added_at_ns = excluded.added_at_ns,
             added_at_sort = excluded.added_at_sort,
             language = excluded.language,
             size_bytes = excluded.size_bytes,
             size_sort = excluded.size_sort,
             resolution_rank = excluded.resolution_rank,
             hdr = excluded.hdr,
             scraped = excluded.scraped,
             has_nfo = excluded.has_nfo,
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
            aliases,
            metadata_revision,
            release_date,
            rating,
            runtime,
            counts.added_ns,
            added_at_sort,
            language,
            counts.size_bytes,
            counts.resolution_rank,
            i64::from(counts.hdr),
            scraped,
            has_nfo,
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

fn media_identity(
    tx: &Connection,
    media: &str,
) -> Result<(String, String, String), StoreError> {
    let (kind, identity_state): (String, String) = tx
        .query_row(
            "SELECT kind, identity_state FROM media WHERE media_id = ?1",
            params![media],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::Missing(format!("media {media}")))?;
    let mut aliases = serde_json::Map::new();
    for source in ["tmdb", "douban", "tvdb", "bangumi", "anilist"] {
        let id: Option<String> = tx
            .query_row(
                "SELECT external_id FROM media_aliases
                 WHERE media_id = ?1 AND kind = ?2 AND source = ?3 LIMIT 1",
                params![media, kind, source],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            aliases.insert(source.to_string(), Value::String(id));
        }
    }
    Ok((kind, identity_state, Value::Object(aliases).to_string()))
}

/// Merge this Library's valid work NFO fields with the adopted catalogue row.
///
/// Precedence is the documented one: the Library's own NFO wins, the adopted row fills the
/// gaps, and the Media identity is the last resort. Card facts and terms both read this one
/// view so a card can never disagree with its filters.
pub(super) fn facts_for(
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

/// The Library's valid work-scope NFO fields: resolved locale first, then the preferred
/// file's sidecar, then the deterministic `(locale, input_key)` order.
fn nfo_fields_for(
    tx: &Connection,
    lib: &str,
    media: &str,
    preferred: Option<&str>,
) -> Result<MetadataFields, StoreError> {
    let locale: Option<String> = tx
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
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
    let fields_json: Option<String> = tx
        .query_row(
            "SELECT fields_json FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work'
               AND parse_state = 'valid'
             ORDER BY (locale = ?3) DESC, (input_key = ?4) DESC, locale, input_key
             LIMIT 1",
            params![lib, media, locale, preferred_key],
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
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
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
    if let Some(locale) = locale {
        let exact = tx
            .query_row(&format!("{COLS} AND locale = ?2"), params![media, locale], read)
            .optional()?;
        if exact.is_some() {
            return Ok(exact);
        }
    }
    Ok(tx
        .query_row(
            &format!("{COLS} ORDER BY locale LIMIT 1"),
            params![media],
            read,
        )
        .optional()?)
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

fn has_valid_work_nfo(tx: &Connection, lib: &str, media: &str) -> Result<i64, StoreError> {
    Ok(tx
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2
                   AND entity_scope = 'work' AND parse_state = 'valid')",
            params![lib, media],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0))
}

/// Rebuild `library_item_terms` for one card: resolutions, genres, countries, public collections.
fn sync_terms(tx: &Connection, lib: &str, media: &str, facts: &Facts) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_item_terms WHERE library_id = ?1 AND media_id = ?2",
        params![lib, media],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO library_item_terms (library_id, term_kind, term_value, media_id)
         SELECT DISTINCT ?1, 'resolution', f.resolution, ?2
         FROM library_files f
         WHERE f.library_id = ?1 AND f.media_id = ?2 AND f.publication_state = 'published'
           AND f.resolution IS NOT NULL AND TRIM(f.resolution) <> ''",
        params![lib, media],
    )?;
    for key in &facts.genres {
        insert_term(tx, lib, media, "genre", key)?;
    }
    for code in &facts.countries {
        insert_term(tx, lib, media, "country", code)?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO library_item_terms (library_id, term_kind, term_value, media_id)
         SELECT ?1, 'collection', c.id, ?2
         FROM collections c
         JOIN collection_items ci ON ci.collection_id = c.id
         WHERE ci.media_id = ?2 AND c.visibility = 'public' AND c.user_id IS NULL",
        params![lib, media],
    )?;
    Ok(())
}

pub(super) fn insert_term(
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

/// Rank order used by the `resolution` sort/filter and by `library_items.resolution_rank`.
pub(crate) fn resolution_rank(value: &str) -> i64 {
    match value {
        "2160p" | "4k" => 4,
        "1080p" | "1080i" => 3,
        "720p" => 2,
        _ => 1,
    }
}

/// Stable pseudo-random order key: FNV-1a over the media id, zero-padded for text order.
pub(crate) fn shuffle_key(media_id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in media_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
