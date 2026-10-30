//! Coverage checks executed on the already owned Subscribe writer.
use domain::{Coverage, Subscribe, SubscribeId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

pub(super) fn ensure_nonoverlapping(
    conn: &Connection,
    candidate: &Subscribe,
) -> Result<(), StoreError> {
    let (kind, season, first, last) = match candidate.coverage {
        Coverage::Movie => ("movie", None, None, None),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => (
            "tv",
            Some(i64::from(season)),
            Some(i64::from(episode_from)),
            episode_to.map(i64::from),
        ),
    };
    let overlap: Option<String> = conn
        .query_row(
            "SELECT id FROM subscribes
         WHERE media_id = ?1 AND library_id IS ?2 AND id != ?3
           AND coverage_kind = ?4
           AND (?4 = 'movie' OR
                (season = ?5 AND (?7 IS NULL OR episode_from <= ?7)
                 AND (episode_to IS NULL OR episode_to >= ?6)))
         ORDER BY id LIMIT 1",
            params![
                candidate.media_id.to_string(),
                candidate.library_id.map(|id| id.to_string()),
                candidate.id.to_string(),
                kind,
                season,
                first,
                last
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = overlap {
        return Err(StoreError::SubscribeOverlap {
            subscribe_id: id.parse::<SubscribeId>()?,
        });
    }
    Ok(())
}

pub(super) fn insert_row(
    library: &Connection,
    subscribe_conn: &Connection,
    subscribe: &Subscribe,
) -> Result<(), StoreError> {
    let state: Option<String> = library
        .query_row(
            "SELECT identity_state FROM media WHERE media_id = ?1",
            params![subscribe.media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if state.is_none() {
        return Err(StoreError::Missing(format!("Media {}", subscribe.media_id)));
    }
    if let Some(state) = state.as_deref()
        && (state == "merging" || state == "retired")
    {
        return Err(StoreError::Protected(format!(
            "media {} identity is frozen ({state})",
            subscribe.media_id
        )));
    }
    let (coverage_kind, season, episode_from, episode_to) = match &subscribe.coverage {
        Coverage::Movie => ("movie", None, None, None),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => (
            "tv",
            Some(*season as i64),
            Some(*episode_from as i64),
            episode_to.map(|v| v as i64),
        ),
    };
    subscribe_conn.execute(
            "INSERT INTO subscribes (
                id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                subscribe.id.to_string(),
                subscribe.user_id.to_string(),
                subscribe.media_id.to_string(),
                coverage_kind,
                season,
                episode_from,
                episode_to,
                subscribe.fetch_mode.as_str(),
                subscribe.filter_id.to_string(),
                subscribe.wash_cut as i64,
                subscribe.wash_cut_filter_id.map(|id| id.to_string()),
                subscribe.full_season_pack as i64,
                subscribe.downloader_id.map(|id| id.to_string()),
                subscribe.tracking_state,
                subscribe.follow_future as i64,
                subscribe.search_interval_secs as i64,
                subscribe.keep_old_versions as i64,
                subscribe.library_id.map(|id| id.to_string()),
                crate::now_rfc3339(),
                crate::now_rfc3339(),
            ],
        )?;
    Ok(())
}
