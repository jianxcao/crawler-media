//! Card reduction entry points and the published-file facts they aggregate.

use super::*;

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

pub(crate) fn refresh_affected_user_summaries(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<(), StoreError> {
    let users: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT user_id FROM playback_units WHERE media_id = ?1 ORDER BY user_id",
        )?;
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

fn published_files(tx: &Connection, lib: &str, media: &str) -> Result<Vec<FileFact>, StoreError> {
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
///
/// Set-based: one aggregate statement for the counters, one windowed statement for the
/// preferred file. The previous per-unit loop issued two statements per unit inside the
/// caller's transaction, so a scan window over a media with `n` units cost `O(n)` round
/// trips and a 10k-episode series grew super-linearly (measured 336 ms @500 →
/// 17,043 ms @2000 in release). Same rows, same counters, fewer statements.
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

    let (seasons, episodes): (i64, i64) = tx.query_row(
        "SELECT COUNT(DISTINCT CASE WHEN u.season >= 0 THEN u.season END),
                COUNT(DISTINCT CASE WHEN u.season >= 0 AND u.episode >= 0
                                    THEN u.season || ':' || u.episode END)
         FROM library_file_units u
         JOIN library_files f ON f.file_id = u.file_id
         WHERE f.library_id = ?1 AND f.media_id = ?2
           AND f.publication_state = 'published'",
        params![lib, media],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    tx.execute(
        "INSERT INTO library_units
            (library_id, media_id, season, episode, file_count, available_file_count,
             preferred_file_id, quality_json, unit_revision)
         SELECT ?1, ?2, g.season, g.episode, g.file_count, g.available,
                (SELECT u2.file_id
                 FROM library_file_units u2
                 JOIN library_files f2 ON f2.file_id = u2.file_id
                 WHERE f2.library_id = ?1 AND f2.media_id = ?2
                   AND u2.season = g.season AND u2.episode = g.episode
                   AND f2.publication_state = 'published'
                   AND +f2.presence_state = 'present'
                 ORDER BY f2.file_revision DESC, f2.file_id ASC
                 LIMIT 1),
                '{}', 1
         FROM (
             SELECT u.season AS season, u.episode AS episode,
                    COUNT(DISTINCT u.file_id) AS file_count,
                    COUNT(DISTINCT CASE WHEN f.presence_state = 'present'
                                        THEN u.file_id END) AS available
             FROM library_file_units u
             JOIN library_files f ON f.file_id = u.file_id
             WHERE f.library_id = ?1 AND f.media_id = ?2
               AND f.publication_state = 'published'
             GROUP BY u.season, u.episode
         ) g
         WHERE true
         ON CONFLICT(library_id, media_id, season, episode) DO UPDATE SET
             file_count = excluded.file_count,
             available_file_count = excluded.available_file_count,
             preferred_file_id = excluded.preferred_file_id,
             unit_revision = library_units.unit_revision + 1",
        params![lib, media],
    )?;
    Ok((seasons, episodes))
}

/// Drop NFO inputs whose sidecar no longer belongs to a published file of this card.
///
/// `input_key` holds the canonical **sidecar** path, so it cannot be compared with
/// `library_files.path_key` (a video path). An input survives while the sidecar's own
/// directory still holds a published file of this card — `tvshow.nfo` / `season.nfo` keep
/// describing the series while any episode remains — or while the file that observed it is
/// still there.
fn prune_nfo_inputs(tx: &Connection, lib: &str, media: &str) -> Result<(), StoreError> {
    const SIDECAR_DIR: &str = "rtrim(library_metadata_inputs.input_key, replace(library_metadata_inputs.input_key, '/', ''))";
    let sql = format!(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND source_version = 'nfo_v1'
           AND NOT EXISTS (
               SELECT 1 FROM library_files AS f
                WHERE f.library_id = ?1 AND f.media_id = ?2
                  AND f.publication_state = 'published'
                  AND (f.path_key = library_metadata_inputs.path_key
                       OR (length({SIDECAR_DIR}) > 0
                           AND substr(f.path_key, 1, length({SIDECAR_DIR})) = {SIDECAR_DIR}))
           )"
    );
    tx.execute(&sql, params![lib, media])?;
    Ok(())
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
