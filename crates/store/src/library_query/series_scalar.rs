//! Scoped Series facts reduced in SQL without materializing episode vectors.

use domain::{MediaId, ProtocolParent, UserId};
use rusqlite::Connection;
use rusqlite::{params_from_iter, types::Value};
use std::collections::HashSet;

use crate::{Store, StoreError};

#[derive(Clone, Copy, Debug, Default)]
pub struct ProtocolSeriesScalar {
    pub episodes: usize,
    pub seasons: usize,
    pub completed: usize,
    pub latest_created_ns: Option<i64>,
}

impl Store {
    pub fn protocol_series_scalar(
        &self,
        user: UserId,
        media: MediaId,
        parent: &ProtocolParent,
    ) -> Result<ProtocolSeriesScalar, StoreError> {
        let allowed = self.authorized_library_ids(user)?;
        self.with_read(crate::Db::Library, |conn| {
            series_scalar_on(conn, &allowed, user, media, parent)
        })
    }
}

/// Borrowed-connection scalar for B's combined page and fact snapshot.
pub(crate) fn series_scalar_on(
    conn: &Connection,
    allowed: &HashSet<String>,
    user: UserId,
    media: MediaId,
    parent: &ProtocolParent,
) -> Result<ProtocolSeriesScalar, StoreError> {
    if allowed.is_empty() {
        return Ok(ProtocolSeriesScalar::default());
    }
    let mut values: Vec<Value> = vec![
        Value::Text(user.to_string()),
        Value::Text(media.to_string()),
    ];
    let placeholders = (0..allowed.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    values.extend(allowed.iter().cloned().map(Value::Text));
    let scope = match parent {
        ProtocolParent::Library(id) => {
            values.push(Value::Text(id.to_string()));
            " AND f.library_id = ?".to_string()
        }
        ProtocolParent::Folder { root_id } => {
            values.push(Value::Text(root_id.clone()));
            " AND f.root_id = ?".to_string()
        }
        _ => String::new(),
    };
    // Separate full-key seeks prevent an unanalysed database from scanning all
    // of this User/Media's assertions once for every owned episode.
    let sql = format!(
        "WITH scoped_files AS (
               SELECT f.file_id, f.created_ns FROM library_files f
               WHERE f.media_id=?2 AND f.library_id IN ({placeholders})
                 AND f.publication_state='published' AND f.presence_state='present' {scope}
             ), covered_files AS (
               SELECT f.file_id,f.created_ns FROM scoped_files f
               WHERE EXISTS (SELECT 1 FROM library_file_units u WHERE u.file_id=f.file_id
                             AND u.season>=0 AND u.episode>=0)
             ), owned AS (
               SELECT DISTINCT u.season, u.episode FROM library_file_units u
               JOIN covered_files f ON f.file_id=u.file_id
               WHERE u.season>=0 AND u.episode>=0
             ), marks AS (
               SELECT o.season,o.episode,
                 (SELECT candidate.played FROM (
                    SELECT p.played,p.played_changed_at_ms,p.season,p.episode
                    FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2
                      AND p.season=o.season AND p.episode=o.episode AND p.has_played_assertion=1
                    UNION ALL
                    SELECT p.played,p.played_changed_at_ms,p.season,p.episode
                    FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2
                      AND p.season=o.season AND p.episode=-1 AND p.has_played_assertion=1
                    UNION ALL
                    SELECT p.played,p.played_changed_at_ms,p.season,p.episode
                    FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2
                      AND p.season=-1 AND p.episode=-1 AND p.has_played_assertion=1
                  ) candidate
                  ORDER BY candidate.played_changed_at_ms DESC, candidate.season DESC, candidate.episode DESC
                  LIMIT 1) AS asserted,
                 (SELECT p.played FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2
                  AND p.season=o.season AND p.episode=o.episode) AS own
               FROM owned o)
             SELECT (SELECT COUNT(*) FROM owned),
                    (SELECT COUNT(DISTINCT season) FROM owned),
                    (SELECT COUNT(*) FROM marks WHERE COALESCE(asserted,own,0)=1),
                    (SELECT MAX(created_ns) FROM covered_files)"
    );
    let (episodes, seasons, completed, latest_created_ns): (i64, i64, i64, Option<i64>) = conn
        .query_row(&sql, params_from_iter(values), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    Ok(ProtocolSeriesScalar {
        episodes: episodes.max(0) as usize,
        seasons: seasons.max(0) as usize,
        completed: completed.max(0) as usize,
        latest_created_ns,
    })
}
