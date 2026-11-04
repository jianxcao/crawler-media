//! Exact FTS evidence is read once outside the writer, then consumed per Header.
use super::{ExpectedProjection, ProjectionKey, search};
use crate::StoreError;
use rusqlite::{Connection, params, types::Value};
use std::sync::Arc;

pub(crate) mod snapshots;
type Token = (String, String, i64);

pub(crate) struct Document {
    rowid: i64,
    media: String,
    exists: bool,
    tokens: Vec<Token>,
}

/// Called only before a newly opened readonly reader enables query_only.
pub(crate) fn open_reader(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("CREATE VIRTUAL TABLE temp.cm_search_actual_vocab USING fts5vocab(main,library_search_fts,'instance')")?;
    Ok(())
}

fn expected_tokens(
    tx: &Connection,
    expected: &ExpectedProjection,
) -> Result<Vec<Token>, StoreError> {
    let text = match expected
        .card
        .as_ref()
        .and_then(|card| card.get("search_text"))
    {
        Some(Value::Text(text)) => text,
        _ => {
            return Err(StoreError::Invalid(
                "expected search document is missing".into(),
            ));
        }
    };
    // Only a single expected document ever occupies these writer-local tables.
    tx.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS temp.cm_search_expected USING fts5(search_text,people,content='',contentless_delete=1,tokenize='trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS temp.cm_search_expected_vocab USING fts5vocab(temp,cm_search_expected,'instance');")?;
    tx.execute("DELETE FROM temp.cm_search_expected WHERE rowid=1", [])?;
    tx.execute(
        "INSERT INTO temp.cm_search_expected(rowid,search_text,people) VALUES(1,?1,?2)",
        params![text, search::people_text(&expected.people)],
    )?;
    Ok(tx
        .prepare_cached(
            "SELECT term,col,offset FROM temp.cm_search_expected_vocab ORDER BY term,col,offset",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?)
}

pub(super) fn matches(
    tx: &Connection,
    key: ProjectionKey,
    expected: &ExpectedProjection,
    actual: Option<&Document>,
) -> Result<bool, StoreError> {
    let Some(rowid) = search::rowid(tx, key)? else {
        return Ok(expected.card.is_none());
    };
    let Some(actual) = actual else {
        return Ok(false);
    };
    Ok(actual.rowid == rowid
        && actual.media == key.media_id.to_string()
        && actual.exists
        && actual.tokens == expected_tokens(tx, expected)?)
}

/// Preserve the existing same-transaction refresh contract: delete+insert has
/// succeeded, so this page's verification uses the same expected tokenizer.
/// This is a local overlay, not an independent reread of the FTS postings.
pub(super) fn refresh(
    tx: &Connection,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<Option<Arc<Document>>, StoreError> {
    if expected.card.is_none() {
        return Ok(None);
    }
    let rowid = search::rowid(tx, key)?
        .ok_or_else(|| StoreError::Invalid("repaired search card is missing".into()))?;
    Ok(Some(Arc::new(Document {
        rowid,
        media: key.media_id.to_string(),
        exists: true,
        tokens: expected_tokens(tx, expected)?,
    })))
}

/// Orphan postings have no Library owner. This global completion scan remains
/// separate from the per-Header writer work; LIMIT bounds returned damage only.
pub(crate) fn finish(
    tx: &Connection,
    repair: bool,
    limit: usize,
) -> Result<(bool, bool), StoreError> {
    let mut ids = tx.prepare_cached("SELECT f.rowid FROM library_search_fts f LEFT JOIN library_items i ON i.search_rowid=f.rowid WHERE i.search_rowid IS NULL LIMIT ?1")?
        .query_map([(limit+1) as i64],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
    let mismatch = !ids.is_empty();
    let more = repair && ids.len() > limit;
    if repair {
        ids.truncate(limit);
        for id in ids {
            search::remove(tx, Some(id))?;
        }
    }
    Ok((mismatch, more))
}
