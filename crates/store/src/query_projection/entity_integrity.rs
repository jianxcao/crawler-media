//! Operation-scoped overlap expectations are reduced in bounded Media batches.
use super::{ExpectedProjection, ProjectionKey, entities};
use crate::StoreError;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

pub(crate) fn stage(
    tx: &Connection,
    operation: &str,
    key: ProjectionKey,
    expected: &ExpectedProjection,
) -> Result<(), StoreError> {
    let lib = key.library_id.to_string();
    let mut owners = entities::ownership(tx, &key.media_id.to_string())?;
    for libraries in owners.values_mut() {
        libraries.remove(&lib);
    }
    for entity in &expected.entities {
        owners
            .entry(entity.clone())
            .or_default()
            .insert(lib.clone());
    }
    for ((kind, name), libraries) in owners {
        if libraries.len() < 2 || !libraries.contains(&lib) {
            continue;
        }
        let json = serde_json::to_string(&libraries)?;
        tx.execute("INSERT INTO protocol_entity_overlap_rebuild(operation_id,entity_kind,name,owner_libraries_json,member_count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(operation_id,entity_kind,name,owner_libraries_json) DO UPDATE SET member_count=member_count+1",params![operation,kind,name,json])?;
    }
    Ok(())
}
pub(crate) fn finish(
    tx: &Connection,
    operation: &str,
    library: &str,
    repair: bool,
) -> Result<bool, StoreError> {
    let actual=tx.prepare_cached("SELECT entity_kind,name,owner_libraries_json,member_count FROM protocol_entity_overlap_counts o WHERE EXISTS(SELECT 1 FROM json_each(o.owner_libraries_json) WHERE value=?1)")?.query_map([library],|r|Ok(((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?),r.get::<_,i64>(3)?)))?.collect::<Result<BTreeMap<_,_>,_>>()?;
    let expected=tx.prepare_cached("SELECT entity_kind,name,owner_libraries_json,member_count FROM protocol_entity_overlap_rebuild WHERE operation_id=?1")?.query_map([operation],|r|Ok(((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?),r.get::<_,i64>(3)?)))?.collect::<Result<BTreeMap<_,_>,_>>()?;
    let mismatch = actual != expected;
    if repair {
        tx.execute("DELETE FROM protocol_entity_overlap_counts AS o WHERE EXISTS(SELECT 1 FROM json_each(o.owner_libraries_json) WHERE value=?1)",[library])?;
        tx.execute("INSERT INTO protocol_entity_overlap_counts(entity_kind,name,owner_libraries_json,member_count) SELECT entity_kind,name,owner_libraries_json,member_count FROM protocol_entity_overlap_rebuild WHERE operation_id=?1",[operation])?;
    }
    tx.execute(
        "DELETE FROM protocol_entity_overlap_rebuild WHERE operation_id=?1",
        [operation],
    )?;
    Ok(mismatch)
}
