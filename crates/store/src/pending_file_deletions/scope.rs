//! Physical deletion ownership includes sidecars without Library file rows.
use std::path::Path;

use rusqlite::Connection;

use crate::StoreError;

fn roots(conn: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT library_id,path_key FROM library_roots WHERE enabled=1 ORDER BY length(path_key) DESC",
    )?;
    Ok(stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

pub(super) fn owner(conn: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    let key = crate::path_key::canonical_recorded_key(conn, key);
    Ok(roots(conn)?
        .into_iter()
        .find(|(_, root)| Path::new(&key).starts_with(root))
        .map(|(library, _)| library))
}

/// Indexed ranges owned by this Library, excluding deeper independent roots of
/// every kind. Subtract ranges before querying the queue so retained nested
/// intents never force an unbounded scan inside a cleanup writer.
pub(crate) fn cancel_removal_batch(conn: &Connection, library: &str) -> Result<usize, StoreError> {
    let roots = roots(conn)?;
    let mut removed = conn.execute(
        "DELETE FROM pending_file_deletions WHERE path_key IN
         (SELECT path_key FROM pending_file_deletions WHERE library_id=?1 ORDER BY path_key LIMIT 100)",
        [library],
    )?;
    if removed == 100 {
        return Ok(removed);
    }
    for (_, root) in roots.iter().filter(|(owner, _)| owner == library) {
        let low = format!("{}/", root.trim_end_matches('/'));
        let high = crate::path_key::prefix_upper_bound(&low);
        let mut ranges = vec![(low, high)];
        for (_, child) in roots
            .iter()
            .filter(|(owner, child)| owner != library && Path::new(child).starts_with(root))
        {
            let child_low = format!("{}/", child.trim_end_matches('/'));
            let child_high = crate::path_key::prefix_upper_bound(&child_low);
            ranges = ranges
                .into_iter()
                .flat_map(|(low, high)| {
                    if child_high <= low || child_low >= high {
                        return vec![(low, high)];
                    }
                    let mut kept = Vec::new();
                    if low < child_low {
                        kept.push((low, child_low.clone()));
                    }
                    if child_high < high {
                        kept.push((child_high.clone(), high));
                    }
                    kept
                })
                .collect();
        }
        for (low, high) in ranges {
            removed += conn.execute(
                "DELETE FROM pending_file_deletions WHERE path_key IN
                 (SELECT path_key FROM pending_file_deletions WHERE library_id IS NULL AND path_key>=?1 AND path_key<?2
                  ORDER BY path_key LIMIT ?3)",
                rusqlite::params![low, high, 100 - removed],
            )?;
            if removed == 100 {
                return Ok(removed);
            }
        }
    }
    Ok(removed)
}
