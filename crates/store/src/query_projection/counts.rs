use super::types::{CounterMember, Terms};
use crate::StoreError;
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
pub(super) fn members(kind: &str, web: &Terms, protocol: &Terms) -> BTreeSet<CounterMember> {
    let mut result = BTreeSet::from([
        CounterMember::new("browse", "all", "*"),
        CounterMember::new("protocol_work", "all", "*"),
        CounterMember::new("protocol_work", "kind", kind),
    ]);
    for (dimension, value) in web {
        if super::browse::is_static(dimension) {
            result.insert(CounterMember::new("browse", dimension, value));
        }
    }
    for (dimension, value) in protocol {
        result.insert(CounterMember::new("protocol_work", dimension, value));
    }
    result
}
pub(super) fn apply(
    tx: &Connection,
    lib: &str,
    before: &BTreeSet<CounterMember>,
    after: &BTreeSet<CounterMember>,
) -> Result<u64, StoreError> {
    for member in before.difference(after) {
        let changed=tx.execute("UPDATE library_query_counts SET member_count=member_count-1 WHERE library_id=?1 AND surface=?2 AND dimension=?3 AND value=?4 AND member_count>0",params![lib,member.surface,member.dimension,member.value])?;
        if changed != 1 {
            return Err(StoreError::Invalid(format!(
                "query counter underflow in library {lib}: {member:?}"
            )));
        }
        tx.execute("DELETE FROM library_query_counts WHERE library_id=?1 AND surface=?2 AND dimension=?3 AND value=?4 AND member_count=0",params![lib,member.surface,member.dimension,member.value])?;
    }
    for member in after.difference(before) {
        tx.execute("INSERT INTO library_query_counts(library_id,surface,dimension,value,member_count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(library_id,surface,dimension,value) DO UPDATE SET member_count=member_count+1",params![lib,member.surface,member.dimension,member.value])?;
    }
    Ok((before.difference(after).count() + after.difference(before).count()) as u64)
}
