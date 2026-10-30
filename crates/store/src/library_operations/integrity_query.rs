//! Per-Media comparison and bounded count staging, independent of live counters.
use super::integrity_guard::IntegrityOwner;
use crate::{
    StoreError,
    query_projection::{ExpectedProjection, ProjectionKey},
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

pub(super) fn stage(
    tx: &Connection,
    operation: &str,
    expected: &ExpectedProjection,
    key: ProjectionKey,
) -> Result<(), StoreError> {
    for m in &expected.members {
        tx.execute("INSERT INTO library_query_count_rebuild(operation_id,surface,dimension,value,member_count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(operation_id,surface,dimension,value) DO UPDATE SET member_count=member_count+1",params![operation,m.surface,m.dimension,m.value])?;
    }
    crate::query_projection::entity_integrity::stage(tx, operation, key, expected)?;
    Ok(())
}
pub(super) fn finish_counts(
    tx: &Connection,
    owner: &IntegrityOwner,
    library: domain::LibraryId,
    operation: &str,
    repair: bool,
) -> Result<bool, StoreError> {
    owner.validate(tx, library)?;
    let lib = library.to_string();
    let actual = counts(tx, "library_query_counts", "library_id", &lib)?;
    let expected = counts(tx, "library_query_count_rebuild", "operation_id", operation)?;
    let mismatch = actual != expected;
    let overlap_mismatch =
        crate::query_projection::entity_integrity::finish(tx, operation, &lib, repair)?;
    if repair {
        tx.execute(
            "DELETE FROM library_query_counts WHERE library_id=?1",
            [&lib],
        )?;
        tx.execute("INSERT INTO library_query_counts(library_id,surface,dimension,value,member_count) SELECT ?1,surface,dimension,value,member_count FROM library_query_count_rebuild WHERE operation_id=?2",params![lib,operation])?;
    }
    tx.execute(
        "DELETE FROM library_query_count_rebuild WHERE operation_id=?1",
        [operation],
    )?;
    Ok(mismatch || overlap_mismatch)
}
fn counts(
    tx: &Connection,
    table: &str,
    column: &str,
    id: &str,
) -> Result<BTreeMap<(String, String, String), i64>, StoreError> {
    Ok(tx
        .prepare(&format!(
            "SELECT surface,dimension,value,member_count FROM {table} WHERE {column}=?1"
        ))?
        .query_map([id], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?
        .collect::<Result<_, _>>()?)
}
pub(super) fn expected(
    tx: &Connection,
    key: ProjectionKey,
) -> Result<ExpectedProjection, StoreError> {
    crate::query_projection::expected_projection(tx, key)
}
