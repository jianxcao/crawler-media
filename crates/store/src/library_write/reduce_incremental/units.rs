//! Unit-row maintenance for the incremental card reduction.
//!
//! A unit row is the per-`(season, episode)` counterpart of the card counters: it also
//! moves by arithmetic from the one changed file, and its preferred file only needs a
//! targeted query when the preferred file itself stopped being an eligible source.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::{ChangedFile, ItemRow, outranks, read_file_summary_facts};
use crate::StoreError;
use crate::library_write::reduce::unit_counts;

/// Rebuild the `library_units` rows the changed file touches and move the distinct
/// season/episode counts by the exact number of units that appeared or disappeared.
pub(super) fn refresh_units(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    change: &ChangedFile<'_>,
    settled: &mut BTreeSet<(i32, i32)>,
) -> Result<(i64, i64), StoreError> {
    let before_published = change.before.is_some_and(|file| file.published);
    let after_published = change.after.published;
    let mut affected = BTreeSet::new();
    if before_published {
        affected.extend(change.before_units.iter().copied());
    }
    if after_published {
        affected.extend(change.after_units.iter().copied());
    }

    let mut seasons = BTreeSet::new();
    for (season, _) in &affected {
        if *season >= 0 {
            seasons.insert(*season);
        }
    }
    let mut seasons_before = BTreeSet::new();
    for season in &seasons {
        if unit_season_exists(tx, lib, media, *season)? {
            seasons_before.insert(*season);
        }
    }

    for (season, episode) in &affected {
        let before_covers = before_published && change.before_units.contains(&(*season, *episode));
        let after_covers = after_published && change.after_units.contains(&(*season, *episode));
        if (!before_covers && !after_covers) || settled.contains(&(*season, *episode)) {
            continue;
        }
        let current = read_unit_row(tx, lib, media, *season, *episode)?;
        let before_exists = current.is_some();
        if !before_exists {
            // Rebuilt from the batch's final facts below: later files of this
            // batch are already counted in that row.
            settled.insert((*season, *episode));
        }
        let after_exists = apply_unit_delta(
            tx,
            lib,
            media,
            *season,
            *episode,
            current,
            before_covers,
            after_covers,
            change,
        )?;
        if before_exists != after_exists && *season >= 0 && *episode >= 0 {
            if after_exists {
                item.distinct_episodes += 1;
            } else {
                item.distinct_episodes -= 1;
            }
        }
    }

    for season in &seasons {
        let after_exists = unit_season_exists(tx, lib, media, *season)?;
        if after_exists != seasons_before.contains(season) {
            if after_exists {
                item.distinct_seasons += 1;
            } else {
                item.distinct_seasons -= 1;
            }
        }
    }
    Ok((item.distinct_seasons, item.distinct_episodes))
}

/// One `library_units` row's counters.
struct UnitRow {
    file_count: i64,
    available: i64,
    preferred: Option<String>,
}

fn read_unit_row(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
) -> Result<Option<UnitRow>, StoreError> {
    let row = tx
        .query_row(
            "SELECT file_count, available_file_count, preferred_file_id
             FROM library_units
             WHERE library_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
            params![lib, media, season, episode],
            |r| {
                Ok(UnitRow {
                    file_count: r.get(0)?,
                    available: r.get(1)?,
                    preferred: r.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Whether any unit row exists for the season, i.e. whether the card counts the season.
fn unit_season_exists(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
) -> Result<bool, StoreError> {
    let exists: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_units
                       WHERE library_id = ?1 AND media_id = ?2 AND season = ?3)",
        params![lib, media, season],
        |r| r.get(0),
    )?;
    Ok(exists != 0)
}

/// Apply one file's coverage change to one unit row. Returns whether the unit still has a
/// published file after the change.
#[allow(clippy::too_many_arguments)]
fn apply_unit_delta(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
    current: Option<UnitRow>,
    before_covers: bool,
    after_covers: bool,
    change: &ChangedFile<'_>,
) -> Result<bool, StoreError> {
    let before = change.before.filter(|file| file.published);
    let after = change.after.published.then_some(change.after);

    let Some(mut row) = current else {
        // The first file of this unit lands here. Any other shape means the summary lost a
        // row, so rebuild this one unit from the facts instead of guessing.
        let (file_count, available, preferred) = unit_counts(tx, lib, media, season, episode)?;
        if file_count == 0 {
            delete_unit(tx, lib, media, season, episode)?;
            return Ok(false);
        }
        upsert_unit(
            tx,
            lib,
            media,
            season,
            episode,
            file_count,
            available,
            preferred.as_deref(),
        )?;
        return Ok(true);
    };

    if before_covers {
        row.file_count -= 1;
        if before.is_some_and(|file| file.presence == "present") {
            row.available -= 1;
        }
    }
    if after_covers {
        row.file_count += 1;
        if after.is_some_and(|file| file.presence == "present") {
            row.available += 1;
        }
    }

    let eligible = after_covers && after.is_some_and(|file| file.presence == "present");
    if eligible {
        let current = row.preferred.clone();
        match current.as_deref() {
            None => row.preferred = Some(change.after.file_id.clone()),
            Some(current) if current == change.after.file_id => {}
            Some(current) => {
                let current_facts = read_file_summary_facts(tx, current)?;
                let outranks = match current_facts {
                    Some(current_facts)
                        if current_facts.published && current_facts.presence == "present" =>
                    {
                        outranks(change.after, &current_facts)
                    }
                    _ => true,
                };
                if outranks {
                    row.preferred = Some(change.after.file_id.clone());
                }
            }
        }
    } else if row.preferred.as_deref() == Some(change.after.file_id.as_str()) {
        row.preferred = unit_preferred_query(tx, lib, media, season, episode)?;
    }

    if row.file_count <= 0 {
        delete_unit(tx, lib, media, season, episode)?;
        return Ok(false);
    }
    upsert_unit(
        tx,
        lib,
        media,
        season,
        episode,
        row.file_count,
        row.available,
        row.preferred.as_deref(),
    )?;
    Ok(true)
}

fn upsert_unit(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
    file_count: i64,
    available: i64,
    preferred: Option<&str>,
) -> Result<(), StoreError> {
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
        params![
            lib, media, season, episode, file_count, available, preferred
        ],
    )?;
    Ok(())
}

fn delete_unit(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_units
         WHERE library_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
        params![lib, media, season, episode],
    )?;
    Ok(())
}

/// Best present published file covering one unit.
fn unit_preferred_query(
    tx: &Connection,
    lib: &str,
    media: &str,
    season: i32,
    episode: i32,
) -> Result<Option<String>, StoreError> {
    let value = tx
        .query_row(
            "SELECT u.file_id
             FROM library_file_units u
             JOIN library_files f ON f.file_id = u.file_id
             WHERE f.library_id = ?1 AND f.media_id = ?2
               AND u.season = ?3 AND u.episode = ?4
               AND f.publication_state = 'published' AND f.presence_state = 'present'
             ORDER BY f.file_revision DESC, f.file_id ASC
             LIMIT 1",
            params![lib, media, season, episode],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value)
}
