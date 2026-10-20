//! Incremental card reduction for a single changed file.
//!
//! [`super::reduce::reduce_library_media`] rebuilds a card from every published file. That is
//! the right shape for repair and batch paths, but it is quadratic when a scan publishes
//! files one at a time. This module updates the same rows from the one file that changed:
//! counters move by arithmetic, and the aggregates arithmetic cannot maintain are re-read
//! with targeted indexed queries over the changed file's card only.

use std::cmp::Ordering;

use domain::{LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::StoreError;
use super::reduce::{
    Counts, clear_summaries, facts_for, reduce_library_media, resolution_rank, upsert_item,
};

mod terms;
mod units;

#[cfg(test)]
mod tests;

/// Facts of one file row that a card summary derives from.
#[derive(Clone, Debug)]
pub(crate) struct FileSummaryFacts {
    pub file_id: String,
    pub path_key: String,
    pub published: bool,
    pub presence: String,
    pub size_bytes: Option<i64>,
    pub resolution: Option<String>,
    pub hdr: bool,
    pub has_still: bool,
    pub poster: bool,
    pub backdrop: bool,
    pub added_ns: Option<i64>,
    pub revision: i64,
}

/// The one file a card reduction is allowed to inspect.
pub(crate) struct ChangedFile<'a> {
    pub before: Option<&'a FileSummaryFacts>,
    pub before_units: &'a [(i32, i32)],
    pub after: &'a FileSummaryFacts,
    pub after_units: &'a [(i32, i32)],
}

/// Read the summary-relevant facts of one file row.
pub(crate) fn read_file_summary_facts(
    tx: &Connection,
    file_id: &str,
) -> Result<Option<FileSummaryFacts>, StoreError> {
    let facts = tx
        .query_row(
            "SELECT file_id, path_key, publication_state, presence_state, size_bytes,
                    resolution, hdr, has_still, has_poster, has_backdrop,
                    COALESCE(created_ns, mtime_ns), file_revision
             FROM library_files WHERE file_id = ?1",
            params![file_id],
            |r| {
                let hdr: Option<String> = r.get(6)?;
                Ok(FileSummaryFacts {
                    file_id: r.get(0)?,
                    path_key: r.get(1)?,
                    published: r.get::<_, String>(2)? == "published",
                    presence: r.get(3)?,
                    size_bytes: r.get(4)?,
                    resolution: r.get(5)?,
                    hdr: hdr.is_some_and(|value| !value.trim().is_empty()),
                    has_still: r.get::<_, i64>(7)? != 0,
                    poster: r.get::<_, i64>(8)? != 0,
                    backdrop: r.get::<_, i64>(9)? != 0,
                    added_ns: r.get(10)?,
                    revision: r.get(11)?,
                })
            },
        )
        .optional()?;
    Ok(facts)
}

/// Coverage units currently recorded for one file.
pub(crate) fn read_file_units(
    tx: &Connection,
    file_id: &str,
) -> Result<Vec<(i32, i32)>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT season, episode FROM library_file_units
         WHERE file_id = ?1 ORDER BY season, episode",
    )?;
    let rows = stmt
        .query_map(params![file_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The card counters the delta arithmetic maintains, plus the two distinct unit counts.
struct ItemRow {
    counts: Counts,
    preferred: Option<String>,
    distinct_seasons: i64,
    distinct_episodes: i64,
}

fn read_item_row(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Option<ItemRow>, StoreError> {
    let row = tx
        .query_row(
            "SELECT file_count, available_file_count, missing_file_count, unknown_file_count,
                    size_bytes, has_still, hdr, resolution_rank, added_at_ns,
                    preferred_file_id, poster_ref, backdrop_ref,
                    distinct_season_count, distinct_episode_count
             FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            params![lib, media],
            |r| {
                Ok(ItemRow {
                    counts: Counts {
                        file: r.get(0)?,
                        available: r.get(1)?,
                        missing: r.get(2)?,
                        unknown: r.get(3)?,
                        size_bytes: r.get(4)?,
                        has_still: r.get::<_, i64>(5)? != 0,
                        hdr: r.get::<_, i64>(6)? != 0,
                        resolution_rank: r.get(7)?,
                        added_ns: r.get(8)?,
                        poster_ref: r.get(10)?,
                        backdrop_ref: r.get(11)?,
                    },
                    preferred: r.get(9)?,
                    distinct_seasons: r.get(12)?,
                    distinct_episodes: r.get(13)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Apply one changed file to `(library_id, media_id)` without re-reading the other files.
pub(crate) fn reduce_library_media_incremental(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
    change: &ChangedFile<'_>,
) -> Result<(), StoreError> {
    let lib = library_id.to_string();
    let media = media_id.to_string();
    let Some(mut item) = read_item_row(tx, &lib, &media)? else {
        if change.after.published {
            // A published file with no card is an inconsistent summary: repair it fully.
            return reduce_library_media(tx, library_id, media_id);
        }
        return Ok(());
    };

    apply_delta(tx, &lib, &media, &mut item, change)?;
    if item.counts.file == 0 {
        clear_summaries(tx, &lib, &media)?;
        return Ok(());
    }
    prune_unpublished_input(tx, &lib, &media, change.after)?;
    let (distinct_seasons, distinct_episodes) =
        units::refresh_units(tx, &lib, &media, &mut item, change)?;
    let facts = facts_for(tx, &lib, &media, item.preferred.as_deref())?;
    upsert_item(
        tx,
        &lib,
        &media,
        &item.counts,
        item.preferred.as_deref(),
        distinct_seasons,
        distinct_episodes,
        &facts,
    )?;
    terms::sync_terms_delta(tx, &lib, &media, &facts, change)?;
    Ok(())
}

fn apply_delta(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    change: &ChangedFile<'_>,
) -> Result<(), StoreError> {
    let before = change.before.filter(|file| file.published);
    let after = change.after.published.then_some(change.after);

    if let Some(before) = before {
        item.counts.file -= 1;
        match before.presence.as_str() {
            "present" => item.counts.available -= 1,
            "missing" => item.counts.missing -= 1,
            _ => item.counts.unknown -= 1,
        }
    }
    if let Some(after) = after {
        item.counts.file += 1;
        match after.presence.as_str() {
            "present" => item.counts.available += 1,
            "missing" => item.counts.missing += 1,
            _ => item.counts.unknown += 1,
        }
    }

    apply_size(tx, lib, media, item, before, after)?;
    apply_flag(
        tx,
        lib,
        media,
        &mut item.counts.has_still,
        before.is_some_and(|file| file.has_still),
        after.is_some_and(|file| file.has_still),
        &change.after.file_id,
        "has_still",
    )?;
    apply_flag(
        tx,
        lib,
        media,
        &mut item.counts.hdr,
        before.is_some_and(|file| file.hdr),
        after.is_some_and(|file| file.hdr),
        &change.after.file_id,
        "hdr",
    )?;
    apply_resolution(tx, lib, media, item, before, after, &change.after.file_id)?;
    apply_added(tx, lib, media, item, before, after, &change.after.file_id)?;
    update_preferred(tx, lib, media, item, change.after)?;

    let poster = item.counts.poster_ref.take();
    item.counts.poster_ref = update_art(tx, lib, media, poster, change.after, true)?;
    let backdrop = item.counts.backdrop_ref.take();
    item.counts.backdrop_ref = update_art(tx, lib, media, backdrop, change.after, false)?;
    Ok(())
}

/// Sum of known sizes: subtract the old contribution, add the new one, and keep the
/// documented NULL-when-nothing-is-known shape.
fn apply_size(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    before: Option<&FileSummaryFacts>,
    after: Option<&FileSummaryFacts>,
) -> Result<(), StoreError> {
    let mut changed = false;
    if let (Some(total), Some(size)) = (item.counts.size_bytes, before.and_then(|f| f.size_bytes)) {
        item.counts.size_bytes = Some(total - size);
        changed = true;
    }
    if let Some(size) = after.and_then(|f| f.size_bytes) {
        item.counts.size_bytes = Some(item.counts.size_bytes.unwrap_or(0) + size);
        changed = true;
    }
    if changed && item.counts.size_bytes == Some(0) {
        let known: i64 = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files
                           WHERE library_id = ?1 AND media_id = ?2
                             AND publication_state = 'published' AND size_bytes IS NOT NULL)",
            params![lib, media],
            |r| r.get(0),
        )?;
        if known == 0 {
            item.counts.size_bytes = None;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_flag(
    tx: &Connection,
    lib: &str,
    media: &str,
    current: &mut bool,
    before: bool,
    after: bool,
    file_id: &str,
    column: &str,
) -> Result<(), StoreError> {
    if before && !after {
        *current = other_file_has(tx, lib, media, file_id, column)?;
    }
    if after {
        *current = true;
    }
    Ok(())
}

fn other_file_has(
    tx: &Connection,
    lib: &str,
    media: &str,
    file_id: &str,
    column: &str,
) -> Result<bool, StoreError> {
    let predicate = if column == "hdr" {
        "hdr IS NOT NULL AND TRIM(hdr) <> ''"
    } else {
        "has_still = 1"
    };
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM library_files
                       WHERE library_id = ?1 AND media_id = ?2
                         AND publication_state = 'published' AND {predicate}
                         AND file_id <> ?3)"
    );
    let exists: i64 = tx.query_row(&sql, params![lib, media, file_id], |r| r.get(0))?;
    Ok(exists != 0)
}

fn apply_resolution(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    before: Option<&FileSummaryFacts>,
    after: Option<&FileSummaryFacts>,
    file_id: &str,
) -> Result<(), StoreError> {
    let rank_of = |file: Option<&FileSummaryFacts>| {
        file.map(|file| resolution_rank(file.resolution.as_deref().unwrap_or("")))
            .unwrap_or(0)
    };
    let before_rank = rank_of(before);
    let after_rank = rank_of(after);
    if before_rank == item.counts.resolution_rank && after_rank < item.counts.resolution_rank {
        item.counts.resolution_rank = max_resolution_rank(tx, lib, media, file_id)?;
    }
    item.counts.resolution_rank = item.counts.resolution_rank.max(after_rank).max(1);
    Ok(())
}

fn max_resolution_rank(
    tx: &Connection,
    lib: &str,
    media: &str,
    file_id: &str,
) -> Result<i64, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT resolution FROM library_files
         WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'
           AND resolution IS NOT NULL AND TRIM(resolution) <> '' AND file_id <> ?3",
    )?;
    let values = stmt
        .query_map(params![lib, media, file_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values
        .iter()
        .map(|value| resolution_rank(value))
        .max()
        .unwrap_or(1))
}

fn apply_added(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    before: Option<&FileSummaryFacts>,
    after: Option<&FileSummaryFacts>,
    file_id: &str,
) -> Result<(), StoreError> {
    let before_added = before.and_then(|file| file.added_ns);
    let after_added = after.and_then(|file| file.added_ns);
    if let Some(old) = before_added
        && item.counts.added_ns == Some(old)
        && after_added.map_or(true, |new| new > old)
    {
        item.counts.added_ns = min_added_ns(tx, lib, media, file_id)?;
    }
    if let Some(new) = after_added {
        item.counts.added_ns = Some(item.counts.added_ns.map_or(new, |current| current.min(new)));
    }
    Ok(())
}

fn min_added_ns(
    tx: &Connection,
    lib: &str,
    media: &str,
    file_id: &str,
) -> Result<Option<i64>, StoreError> {
    let value = tx.query_row(
        "SELECT MIN(COALESCE(created_ns, mtime_ns)) FROM library_files
         WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'
           AND file_id <> ?3",
        params![lib, media, file_id],
        |r| r.get(0),
    )?;
    Ok(value)
}

fn update_preferred(
    tx: &Connection,
    lib: &str,
    media: &str,
    item: &mut ItemRow,
    after: &FileSummaryFacts,
) -> Result<(), StoreError> {
    let eligible = after.published && after.presence == "present";
    let current = item.preferred.clone();
    match current.as_deref() {
        None => {
            if eligible {
                item.preferred = Some(after.file_id.clone());
            }
        }
        Some(current) if current == after.file_id => {
            if !eligible {
                item.preferred = preferred_query(tx, lib, media)?;
            }
        }
        Some(current) => {
            if eligible {
                let current_facts = read_file_summary_facts(tx, current)?;
                let outranks = match current_facts {
                    Some(current_facts)
                        if current_facts.published && current_facts.presence == "present" =>
                    {
                        outranks(after, &current_facts)
                    }
                    _ => true,
                };
                if outranks {
                    item.preferred = Some(after.file_id.clone());
                }
            }
        }
    }
    Ok(())
}

/// `ORDER BY file_revision DESC, file_id ASC` among published + present files.
fn preferred_query(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<Option<String>, StoreError> {
    let value = tx
        .query_row(
            "SELECT file_id FROM library_files
             WHERE library_id = ?1 AND media_id = ?2
               AND publication_state = 'published' AND presence_state = 'present'
             ORDER BY file_revision DESC, file_id ASC
             LIMIT 1",
            params![lib, media],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value)
}

fn update_art(
    tx: &Connection,
    lib: &str,
    media: &str,
    current: Option<String>,
    after: &FileSummaryFacts,
    poster: bool,
) -> Result<Option<String>, StoreError> {
    let qualifies = after.published
        && if poster {
            after.poster
        } else {
            after.backdrop
        };
    let Some(reference) = current else {
        return Ok(qualifies.then(|| after.file_id.clone()));
    };
    if reference == after.file_id {
        if qualifies && after.presence == "present" {
            return Ok(Some(reference));
        }
        return art_query(tx, lib, media, poster);
    }
    if qualifies {
        let current_facts = read_file_summary_facts(tx, &reference)?;
        let outranks = match current_facts {
            Some(current_facts)
                if current_facts.published
                    && if poster {
                        current_facts.poster
                    } else {
                        current_facts.backdrop
                    } =>
            {
                outranks(after, &current_facts)
            }
            _ => true,
        };
        if outranks {
            return Ok(Some(after.file_id.clone()));
        }
    }
    Ok(Some(reference))
}

/// Best published file carrying the requested artwork, present files first.
fn art_query(
    tx: &Connection,
    lib: &str,
    media: &str,
    poster: bool,
) -> Result<Option<String>, StoreError> {
    let column = if poster { "has_poster" } else { "has_backdrop" };
    let sql = format!(
        "SELECT file_id FROM library_files
         WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published'
           AND {column} = 1
         ORDER BY (presence_state = 'present') DESC, file_revision DESC, file_id ASC
         LIMIT 1"
    );
    let value = tx
        .query_row(&sql, params![lib, media], |r| r.get(0))
        .optional()?;
    Ok(value)
}

/// Present beats missing, then newest revision, then the smaller file id.
fn outranks(candidate: &FileSummaryFacts, current: &FileSummaryFacts) -> bool {
    match (candidate.presence == "present").cmp(&(current.presence == "present")) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => {
            candidate.revision > current.revision
                || (candidate.revision == current.revision && candidate.file_id < current.file_id)
        }
    }
}

/// A file that stopped being published can no longer own its NFO input.
fn prune_unpublished_input(
    tx: &Connection,
    lib: &str,
    media: &str,
    after: &FileSummaryFacts,
) -> Result<(), StoreError> {
    if after.published {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND source_version = 'nfo_v1'
           AND input_key = ?3",
        params![lib, media, after.path_key],
    )?;
    Ok(())
}
