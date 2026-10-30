//! Explicit maintenance snapshots token instances once per operation/connection.
//!
//! fts5vocab cannot seek a doc id. Copying it once into an indexed TEMP relation
//! avoids a whole-vocabulary scan per card. Normal readers/writers never do this.
use super::{ExpectedProjection, ProjectionKey, search};
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params, types::Value};

type Token = (String, String, i64);
const TEMP_DDL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS temp.cm_search_actual_vocab USING fts5vocab(main,library_search_fts,'instance');
CREATE VIRTUAL TABLE IF NOT EXISTS temp.cm_search_expected USING fts5(search_text,people,content='',contentless_delete=1,tokenize='trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS temp.cm_search_expected_vocab USING fts5vocab(temp,cm_search_expected,'instance');
CREATE TEMP TABLE IF NOT EXISTS cm_search_scopes(operation TEXT PRIMARY KEY,library TEXT NOT NULL UNIQUE);
CREATE TEMP TABLE IF NOT EXISTS cm_search_docs(operation TEXT NOT NULL,doc INTEGER NOT NULL,PRIMARY KEY(operation,doc)) WITHOUT ROWID;
CREATE TEMP TABLE IF NOT EXISTS cm_search_tokens(operation TEXT NOT NULL,doc INTEGER NOT NULL,term TEXT NOT NULL,col TEXT NOT NULL,offset INTEGER NOT NULL,PRIMARY KEY(operation,doc,term,col,offset)) WITHOUT ROWID;";

pub(crate) fn prepare(tx: &Connection, operation: &str, library: &str) -> Result<(), StoreError> {
    tx.execute_batch(TEMP_DDL)?;
    let ready: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM temp.cm_search_scopes WHERE operation=?1 AND library=?2)",
        params![operation, library],
        |r| r.get(0),
    )?;
    if ready {
        return Ok(());
    }
    // CROSS JOIN fixes one vocabulary pass, followed by integer-rowid seeks.
    tx.execute("INSERT INTO temp.cm_search_tokens SELECT ?1,v.doc,v.term,v.col,v.offset FROM temp.cm_search_actual_vocab v CROSS JOIN library_items i ON i.search_rowid=v.doc WHERE i.library_id=?2",params![operation,library])?;
    tx.execute("INSERT INTO temp.cm_search_docs SELECT ?1,f.rowid FROM library_search_fts f CROSS JOIN library_items i ON i.search_rowid=f.rowid WHERE i.library_id=?2",params![operation,library])?;
    tx.execute(
        "INSERT INTO temp.cm_search_scopes(operation,library) VALUES(?1,?2)",
        params![operation, library],
    )?;
    Ok(())
}
fn operation(tx: &Connection, key: ProjectionKey) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT operation FROM temp.cm_search_scopes WHERE library=?1",
        [key.library_id.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::Protected("search Integrity snapshot is not prepared".into()))
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
) -> Result<bool, StoreError> {
    let Some(rowid) = search::rowid(tx, key)? else {
        return Ok(expected.card.is_none());
    };
    let operation = operation(tx, key)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM temp.cm_search_docs WHERE operation=?1 AND doc=?2)",
        params![operation, rowid],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(false);
    }
    let actual = tx.prepare_cached("SELECT term,col,offset FROM temp.cm_search_tokens WHERE operation=?1 AND doc=?2 ORDER BY term,col,offset")?
        .query_map(params![operation,rowid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<Result<Vec<Token>,_>>()?;
    Ok(actual == expected_tokens(tx, expected)?)
}

/// Exact delete+insert already succeeded in this same transaction; maintain its
/// operation snapshot with the same tokenizer, so later pages see the repair.
pub(super) fn refresh(
    tx: &Connection,
    key: ProjectionKey,
    old: Option<i64>,
    expected: &ExpectedProjection,
) -> Result<(), StoreError> {
    let operation = operation(tx, key)?;
    if let Some(rowid) = old {
        tx.execute(
            "DELETE FROM temp.cm_search_docs WHERE operation=?1 AND doc=?2",
            params![operation, rowid],
        )?;
        tx.execute(
            "DELETE FROM temp.cm_search_tokens WHERE operation=?1 AND doc=?2",
            params![operation, rowid],
        )?;
    }
    if expected.card.is_some() {
        let rowid = search::rowid(tx, key)?
            .ok_or_else(|| StoreError::Invalid("repaired search card is missing".into()))?;
        tx.execute(
            "INSERT INTO temp.cm_search_docs(operation,doc) VALUES(?1,?2)",
            params![operation, rowid],
        )?;
        for (term, col, offset) in expected_tokens(tx, expected)? {
            tx.execute("INSERT INTO temp.cm_search_tokens(operation,doc,term,col,offset) VALUES(?1,?2,?3,?4,?5)",params![operation,rowid,term,col,offset])?;
        }
    }
    Ok(())
}

/// Rebuild even clean documents during an explicit repair operation.
pub(crate) fn rewrite_document(
    tx: &Connection,
    owner: &crate::library_operations::integrity_guard::IntegrityOwner,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<(), StoreError> {
    owner.validate(tx, key.library_id)?;
    let old = search::rowid(tx, key)?;
    search::remove(tx, old)?;
    search::sync(tx, key, &expected.people)?;
    refresh(tx, key, old, expected)
}

/// Orphan postings have no Library owner. Explicit maintenance may remove a
/// bounded page globally; ordinary writes cannot publish such a half-state.
pub(crate) fn finish(
    tx: &Connection,
    operation: &str,
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
    if !more {
        tx.execute(
            "DELETE FROM temp.cm_search_tokens WHERE operation=?1",
            [operation],
        )?;
        tx.execute(
            "DELETE FROM temp.cm_search_docs WHERE operation=?1",
            [operation],
        )?;
        tx.execute(
            "DELETE FROM temp.cm_search_scopes WHERE operation=?1",
            [operation],
        )?;
    }
    Ok((mismatch, more))
}
