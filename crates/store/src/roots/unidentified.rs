//! Indexed bounded windows of parked identities; display paths never select work.
use domain::MediaKind;
use rusqlite::{Connection, params};

use crate::StoreError;

type Candidate = (String, String, String, String, String, i64, String);

pub(super) fn reassign_batch(
    tx: &Connection,
    kind: MediaKind,
    root_key: &str,
) -> Result<Option<bool>, StoreError> {
    let (cursor, cursor_id): (String, String) = tx.query_row(
        "SELECT unidentified_cursor_key,unidentified_cursor_id FROM pending_root_reassignments
         WHERE kind=?1 AND root_key=?2",
        params![kind.as_str(), root_key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let candidates = candidates(tx, root_key, &cursor, &cursor_id)?;
    for (owner, root, _path, key, confidence, _revision, _) in &candidates {
        let target: Option<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT root_id,library_id FROM library_roots WHERE enabled=1 AND
                 (path_key='/' OR ?1=path_key OR substr(?1,1,length(path_key)+1)=path_key||'/')
                 ORDER BY length(path_key) DESC LIMIT 1",
            )?;
            use rusqlite::OptionalExtension;
            stmt.query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
        };
        let Some((new_root, target)) = target else {
            continue;
        };
        if owner == &target && root == &new_root {
            continue;
        }
        if !super::library_is_ready(tx, owner)? || !super::library_is_ready(tx, &target)? {
            return Ok(None);
        }
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM unidentified WHERE library_id=?1 AND path_key=?2)",
            params![target, key],
            |r| r.get(0),
        )?;
        if owner == &target || !target_exists {
            tx.execute(
                "UPDATE unidentified SET library_id=?1,root_id=?2,revision=revision+1
                 WHERE library_id=?3 AND path_key=?4",
                params![target, new_root, owner, key],
            )?;
        } else {
            tx.execute(
                "UPDATE unidentified SET root_id=?1,confidence=CASE WHEN confidence='low' OR ?2='low'
                 THEN 'low' ELSE 'high' END,revision=revision+1 WHERE library_id=?3 AND path_key=?4",
                params![new_root,confidence,target,key],
            )?;
            tx.execute(
                "DELETE FROM unidentified WHERE library_id=?1 AND path_key=?2",
                params![owner, key],
            )?;
        }
    }
    if let Some((_, _, _, key, _, _, identity)) = candidates.last() {
        tx.execute(
            "UPDATE pending_root_reassignments SET unidentified_cursor_key=?3,unidentified_cursor_id=?4
             WHERE kind=?1 AND root_key=?2", params![kind.as_str(),root_key,key,identity],
        )?;
        tracing::info!(
            root_key,
            count = candidates.len(),
            "Unidentified ownership window processed"
        );
    }
    Ok(Some(candidates.len() < super::REASSIGN_BATCH))
}

fn candidates(
    tx: &Connection,
    root: &str,
    cursor: &str,
    cursor_id: &str,
) -> Result<Vec<Candidate>, StoreError> {
    let (low, high) = super::component_range(root);
    let seek = std::cmp::max(low.as_str(), cursor);
    // Both branches seek the path index. UNION sorts at most 101 rows, including
    // a possible exact root identity, never every row that already has its owner.
    let mut stmt = tx.prepare(
        "WITH exact AS MATERIALIZED (
           SELECT library_id,root_id,path,path_key,confidence,revision,unidentified_id FROM unidentified
           WHERE path_key=?1 AND (path_key,unidentified_id)>(?4,?5) LIMIT 100
         ), children AS MATERIALIZED (
           SELECT library_id,root_id,path,path_key,confidence,revision,unidentified_id FROM unidentified
           WHERE path_key<>?1 AND path_key>=?2 AND path_key<?3 AND (path_key,unidentified_id)>(?4,?5)
           ORDER BY path_key,unidentified_id LIMIT 100
         )
         SELECT * FROM (SELECT * FROM exact UNION ALL SELECT * FROM children)
         ORDER BY path_key,unidentified_id LIMIT 100",
    )?;
    let rows = stmt.query_map(params![root, seek, high, cursor, cursor_id], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

#[cfg(test)]
mod tests;
