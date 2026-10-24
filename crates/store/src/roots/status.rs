//! Latest durable lifecycle per root. A new intent replaces the terminal history,
//! so observability stays proportional to configured roots, rather than batches.
use domain::MediaKind;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RootReassignment {
    pub operation_id: String,
    pub library_id: String,
    pub root_key: String,
    pub config_revision: i64,
    pub state: String,
    pub revision: i64,
    pub error: Option<String>,
    pub updated_at_ms: i64,
}

pub(super) fn register(
    tx: &Connection,
    library: &str,
    kind: MediaKind,
    root: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO root_reassignment_state(kind,root_key,operation_id,library_id,config_revision,
         state,revision,error,updated_at_ms)
         SELECT ?1,?2,?3,?4,config_revision,'queued',1,NULL,?5 FROM libraries WHERE library_id=?4
         ON CONFLICT(kind,root_key) DO UPDATE SET operation_id=CASE WHEN EXISTS
           (SELECT 1 FROM pending_root_reassignments WHERE kind=?1 AND root_key=?2)
           THEN root_reassignment_state.operation_id ELSE excluded.operation_id END,
           library_id=excluded.library_id,config_revision=excluded.config_revision,
           state='queued',revision=root_reassignment_state.revision+1,error=NULL,updated_at_ms=excluded.updated_at_ms",
        params![kind.as_str(),root,uuid::Uuid::new_v4().to_string(),library,crate::unix_now()*1000],
    )?;
    Ok(())
}

pub(super) fn update(
    tx: &Connection,
    kind: MediaKind,
    root: &str,
    state: &str,
    error: Option<&str>,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE root_reassignment_state SET state=?3,error=?4,revision=revision+1,updated_at_ms=?5
         WHERE kind=?1 AND root_key=?2 AND (state<>?3 OR error IS NOT ?4 OR ?3='running')",
        params![kind.as_str(), root, state, error, crate::unix_now() * 1000],
    )?;
    Ok(())
}

impl Store {
    /// Query the durable operation after a Library create or a driver restart.
    pub fn root_reassignments_for_library(
        &self,
        library: &str,
    ) -> Result<Vec<RootReassignment>, StoreError> {
        let mut statement=self.library.prepare(
            "SELECT operation_id,library_id,root_key,config_revision,state,revision,error,updated_at_ms
             FROM root_reassignment_state WHERE library_id=?1 ORDER BY root_key",
        )?;
        let rows = statement.query_map([library], |row| {
            Ok(RootReassignment {
                operation_id: row.get(0)?,
                library_id: row.get(1)?,
                root_key: row.get(2)?,
                config_revision: row.get(3)?,
                state: row.get(4)?,
                revision: row.get(5)?,
                error: row.get(6)?,
                updated_at_ms: row.get(7)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}
