//! Transactional search documents share the card's stable SQLite rowid.
use super::{ProjectionKey, types::Person};
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

pub(super) fn rowid(tx: &Connection, key: ProjectionKey) -> Result<Option<i64>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT search_rowid FROM library_items WHERE library_id=?1 AND media_id=?2",
            params![key.library_id.to_string(), key.media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?)
}
pub(super) fn people_text(people: &BTreeSet<Person>) -> String {
    people
        .iter()
        .map(|p| p.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\u{1f}")
}
pub(super) fn remove(tx: &Connection, rowid: Option<i64>) -> Result<(), StoreError> {
    if let Some(rowid) = rowid {
        tx.execute("DELETE FROM library_search_fts WHERE rowid=?1", [rowid])?;
    }
    Ok(())
}
pub(super) fn sync(
    tx: &Connection,
    key: ProjectionKey,
    people: &BTreeSet<Person>,
) -> Result<(), StoreError> {
    let row: Option<(i64,String)> = tx.query_row(
        "SELECT search_rowid,search_text FROM library_items WHERE library_id=?1 AND media_id=?2",
        params![key.library_id.to_string(),key.media_id.to_string()], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((rowid, text)) = row else {
        return Ok(());
    };
    remove(tx, Some(rowid))?;
    tx.execute(
        "INSERT INTO library_search_fts(rowid,search_text,people) VALUES(?1,?2,?3)",
        params![rowid, text, people_text(people)],
    )?;
    Ok(())
}

/// Orphan purge also handles derived cards left behind by corrupt raw facts.
pub(crate) fn remove_media(tx: &Connection, media: &str) -> Result<(), StoreError> {
    let ids = tx
        .prepare_cached("SELECT search_rowid FROM library_items WHERE media_id=?1")?
        .query_map([media], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in ids {
        remove(tx, Some(id))?;
    }
    Ok(())
}
