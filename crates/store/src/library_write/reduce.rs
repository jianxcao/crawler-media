use domain::{LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::{Connection, params};

use super::super::files::LibraryItemSummary;
use super::super::StoreError;

pub fn reduce_library_media(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<(), StoreError> {
    let lib_id_str = library_id.to_string();
    let media_id_str = media_id.to_string();

    // 1. Gather all published / pending / missing files for this (library_id, media_id)
    struct FileRow {
        file_id: String,
        presence_state: String,
        publication_state: String,
        size_bytes: Option<i64>,
        resolution: Option<String>,
        has_poster: bool,
        has_backdrop: bool,
        has_still: bool,
    }

    let mut stmt = tx.prepare(
        "SELECT file_id, presence_state, publication_state, size_bytes, resolution, has_poster, has_backdrop, has_still
         FROM library_files
         WHERE library_id = ?1 AND media_id = ?2",
    )?;
    let file_rows = stmt
        .query_map(params![lib_id_str, media_id_str], |r| {
            Ok(FileRow {
                file_id: r.get(0)?,
                presence_state: r.get(1)?,
                publication_state: r.get(2)?,
                size_bytes: r.get(3)?,
                resolution: r.get(4)?,
                has_poster: r.get::<_, i64>(5)? != 0,
                has_backdrop: r.get::<_, i64>(6)? != 0,
                has_still: r.get::<_, i64>(7)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if file_rows.is_empty() {
        // Delete library_item if no files remain
        tx.execute(
            "DELETE FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            params![lib_id_str, media_id_str],
        )?;
        tx.execute(
            "DELETE FROM library_units WHERE library_id = ?1 AND media_id = ?2",
            params![lib_id_str, media_id_str],
        )?;
        return Ok(());
    }

    let file_count = file_rows.len();
    let mut available_file_count = 0;
    let mut missing_file_count = 0;
    let mut unknown_file_count = 0;
    let mut total_size: i64 = 0;
    let mut preferred_file_id: Option<String> = None;
    let mut has_still = false;

    for f in &file_rows {
        if let Some(s) = f.size_bytes {
            total_size += s;
        }
        if f.has_still {
            has_still = true;
        }
        if f.publication_state == "published" {
            match f.presence_state.as_str() {
                "present" => {
                    available_file_count += 1;
                    if preferred_file_id.is_none() {
                        preferred_file_id = Some(f.file_id.clone());
                    }
                }
                "missing" => missing_file_count += 1,
                _ => unknown_file_count += 1,
            }
        } else {
            // Pending files are counted in unknown_file_count or missing, but not available
            unknown_file_count += 1;
        }
    }

    // 2. Reduce library_units from library_file_units
    // First delete any units for this (library, media) that have no units in library_file_units
    tx.execute(
        "DELETE FROM library_units
         WHERE library_id = ?1 AND media_id = ?2
           AND (season, episode) NOT IN (
               SELECT season, episode FROM library_file_units
               WHERE library_id = ?1 AND media_id = ?2
           )",
        params![lib_id_str, media_id_str],
    )?;

    let mut unit_stmt = tx.prepare(
        "SELECT season, episode, COUNT(DISTINCT file_id)
         FROM library_file_units
         WHERE library_id = ?1 AND media_id = ?2
         GROUP BY season, episode",
    )?;
    let unit_groups = unit_stmt
        .query_map(params![lib_id_str, media_id_str], |r| {
            Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?, r.get::<_, usize>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut distinct_seasons = std::collections::HashSet::new();
    let mut distinct_episodes = std::collections::HashSet::new();

    for (s, ep, u_file_count) in &unit_groups {
        if *s >= 0 {
            distinct_seasons.insert(*s);
        }
        if *ep >= 0 {
            distinct_episodes.insert((*s, *ep));
        }

        // Find available files for this unit
        let mut u_files_stmt = tx.prepare(
            "SELECT u.file_id
             FROM library_file_units u
             JOIN library_files f ON u.file_id = f.file_id
             WHERE u.library_id = ?1 AND u.media_id = ?2 AND u.season = ?3 AND u.episode = ?4
               AND f.publication_state = 'published' AND f.presence_state = 'present'
             ORDER BY f.file_revision DESC, f.file_id ASC",
        )?;
        let u_avail: Vec<String> = u_files_stmt
            .query_map(params![lib_id_str, media_id_str, s, ep], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let u_avail_count = u_avail.len();
        let u_preferred = u_avail.first().cloned();

        tx.execute(
            "INSERT INTO library_units (library_id, media_id, season, episode, file_count, available_file_count, preferred_file_id, quality_json, unit_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', 1)
             ON CONFLICT(library_id, media_id, season, episode) DO UPDATE SET
                 file_count = excluded.file_count,
                 available_file_count = excluded.available_file_count,
                 preferred_file_id = excluded.preferred_file_id,
                 unit_revision = library_units.unit_revision + 1",
            params![
                lib_id_str,
                media_id_str,
                s,
                ep,
                u_file_count,
                u_avail_count,
                u_preferred,
            ],
        )?;
    }

    // 3. Fetch media details
    let (kind_str, title, year, identity_state): (String, String, Option<i64>, String) = tx.query_row(
        "SELECT kind, title, year, identity_state FROM media WHERE media_id = ?1",
        params![media_id_str],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;

    // Fetch rating and release_date from media_metadata if available
    let (rating_sort, release_date_sort): (f64, String) = tx.query_row(
        "SELECT COALESCE(rating, -1.0), COALESCE(release_date, '') FROM media_metadata WHERE media_id = ?1",
        params![media_id_str],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap_or((-1.0, String::new()));

    let missing_flag = if missing_file_count > 0 { 1 } else { 0 };
    let stable_shuffle_key = format!("{media_id_str}");

    tx.execute(
        "INSERT INTO library_items
            (library_id, media_id, kind, title, title_sort, year, identity_state,
             aliases_json, metadata_revision, item_revision, release_date_sort,
             rating_sort, runtime_sort, added_at_sort, size_bytes, size_sort,
             resolution_rank, hdr, scraped, has_nfo, missing, stable_shuffle_key,
             file_count, available_file_count, missing_file_count, unknown_file_count,
             distinct_episode_count, distinct_season_count, preferred_file_id, has_still)
         VALUES
            (?1, ?2, ?3, ?4, ?4, ?5, ?6,
             '[]', 0, 1, ?18,
             ?19, 0, 0, ?7, ?7,
             0, 0, 0, 0, ?8, ?9,
             ?10, ?11, ?12, ?13,
             ?14, ?15, ?16, ?17)
         ON CONFLICT(library_id, media_id) DO UPDATE SET
             file_count = excluded.file_count,
             available_file_count = excluded.available_file_count,
             missing_file_count = excluded.missing_file_count,
             unknown_file_count = excluded.unknown_file_count,
             distinct_episode_count = excluded.distinct_episode_count,
             distinct_season_count = excluded.distinct_season_count,
             preferred_file_id = excluded.preferred_file_id,
             size_bytes = excluded.size_bytes,
             size_sort = excluded.size_sort,
             missing = excluded.missing,
             has_still = excluded.has_still,
             rating_sort = excluded.rating_sort,
             release_date_sort = excluded.release_date_sort,
             item_revision = library_items.item_revision + 1",
        params![
            lib_id_str,
            media_id_str,
            kind_str,
            title,
            year,
            identity_state,
            total_size,
            missing_flag,
            stable_shuffle_key,
            file_count,
            available_file_count,
            missing_file_count,
            unknown_file_count,
            distinct_episodes.len(),
            distinct_seasons.len(),
            preferred_file_id,
            if has_still { 1 } else { 0 },
            release_date_sort,
            rating_sort,
        ],
    )?;

    Ok(())
}
