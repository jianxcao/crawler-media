//! A bounded ownership pass for Unidentified facts, including paths with no file row.
use rusqlite::{Connection, params};

use crate::StoreError;

struct Reassignment {
    source: String,
    path: String,
    confidence: String,
    root: String,
    target: String,
}

/// Updated ownership is the durable progress marker: a moved row no longer matches
/// the next page. The pending root is retained until no misowned facts remain.
pub(super) fn reassign_batch(tx: &Connection, root_key: &str) -> Result<Option<bool>, StoreError> {
    let (low, high) = super::component_range(root_key);
    let mut statement = tx.prepare(
        "WITH plans AS (
           SELECT u.library_id,u.root_id,u.path,u.confidence,
             (SELECT r.root_id FROM library_roots r
              WHERE r.enabled=1 AND
                (r.path_key='/' OR norm_path_key(u.path)=r.path_key OR
                 substr(norm_path_key(u.path),1,length(r.path_key)+1)=r.path_key||'/')
              ORDER BY length(r.path_key) DESC LIMIT 1) AS target_root
           FROM unidentified u
           WHERE norm_path_key(u.path)=?1 OR
                 (norm_path_key(u.path)>=?2 AND norm_path_key(u.path)<?3)
         )
         SELECT p.library_id,p.path,p.confidence,r.root_id,r.library_id
         FROM plans p JOIN library_roots r ON r.root_id=p.target_root
         WHERE p.library_id<>r.library_id OR p.root_id<>r.root_id
         ORDER BY p.path,p.library_id LIMIT ?4",
    )?;
    let rows: Vec<Reassignment> = statement
        .query_map(
            params![root_key, low, high, super::REASSIGN_BATCH as i64],
            |row| {
                Ok(Reassignment {
                    source: row.get(0)?,
                    path: row.get(1)?,
                    confidence: row.get(2)?,
                    root: row.get(3)?,
                    target: row.get(4)?,
                })
            },
        )?
        .collect::<Result<_, _>>()?;
    for row in &rows {
        if !super::library_is_ready(tx, &row.source)? || !super::library_is_ready(tx, &row.target)?
        {
            return Ok(None);
        }
    }
    for row in &rows {
        if row.source == row.target {
            tx.execute(
                "UPDATE unidentified SET root_id=?1 WHERE library_id=?2 AND path=?3",
                params![row.root, row.source, row.path],
            )?;
        } else {
            tx.execute(
                "INSERT INTO unidentified(library_id,root_id,path,confidence) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(library_id,path) DO UPDATE SET root_id=excluded.root_id,
                   confidence=CASE WHEN confidence='low' OR excluded.confidence='low'
                                   THEN 'low' ELSE 'high' END",
                params![row.target, row.root, row.path, row.confidence],
            )?;
            tx.execute(
                "DELETE FROM unidentified WHERE library_id=?1 AND path=?2",
                params![row.source, row.path],
            )?;
        }
    }
    if !rows.is_empty() {
        tracing::info!(
            root_key,
            count = rows.len(),
            "Unidentified ownership batch reassigned"
        );
    }
    Ok(Some(rows.len() < super::REASSIGN_BATCH))
}
