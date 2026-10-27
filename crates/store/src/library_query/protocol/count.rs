use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

use super::{page::ProtocolReadPlan, scope::Scope};
use crate::{StoreError, library_query::members::CountStrategy};

pub(super) fn count(
    tx: &Connection,
    plan: &ProtocolReadPlan,
    scope: &Scope,
    scoped: &str,
    rows: &str,
    values: &[Value],
) -> Result<u64, StoreError> {
    let total: i64 = match &plan.count_strategy {
        CountStrategy::Marginal { dimension, value } => tx.query_row(
            "SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='protocol_work' AND dimension=?2 AND value=?3",
            params![scope.library_ids[0], dimension, value], |r| r.get(0),
        ).optional()?.unwrap_or(0),
        CountStrategy::ExactMembers => tx.query_row(
            &format!("WITH {scoped}, {rows} SELECT (SELECT COUNT(*) FROM (SELECT 1 FROM rows WHERE season=-1 GROUP BY media_id)) + (SELECT COUNT(*) FROM (SELECT 1 FROM rows WHERE season>=0 GROUP BY media_id,season,episode))"),
            params_from_iter(values.iter()), |r| r.get(0),
        )?,
    };
    Ok(total.max(0) as u64)
}
