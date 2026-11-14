//! Process-local actual tokens, scoped to one durable Integrity owner revision.
use super::{Document, ProjectionKey, Token};
use crate::{StoreError, library_read::active_query_budget};
use parking_lot::{Condvar, Mutex};
use rusqlite::Connection;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct Generation {
    operation: String,
    library: String,
    revision: i64,
}

#[derive(Default)]
pub(crate) struct Snapshots(Mutex<BTreeMap<Generation, Arc<Slot>>>);

impl Snapshots {
    pub(crate) fn document(
        &self,
        conn: &Connection,
        operation: &str,
        key: ProjectionKey,
        revision: i64,
    ) -> Result<Option<Arc<Document>>, StoreError> {
        let library = key.library_id.to_string();
        let generation = Generation {
            operation: operation.into(),
            library: library.clone(),
            revision,
        };
        let slot = self.0.lock().entry(generation).or_default().clone();
        let snapshot = slot.load(conn, &library)?;
        let rowid = super::search::rowid(conn, key)?;
        Ok(rowid
            .and_then(|id| snapshot.0.get(&id))
            .filter(|doc| doc.media == key.media_id.to_string())
            .cloned())
    }

    /// Called after writer release. Large token storage is never dropped under
    /// the registry mutex or the actual database writer.
    pub(crate) fn finish(&self, operation: &str) {
        let removed = {
            let mut entries = self.0.lock();
            let keys: Vec<_> = entries
                .keys()
                .filter(|key| key.operation == operation)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| entries.remove(&key))
                .collect::<Vec<_>>()
        };
        drop(removed);
    }
}

#[derive(Default)]
struct Slot {
    state: Mutex<SlotState>,
    changed: Condvar,
}
#[derive(Default)]
struct SlotState {
    building: bool,
    ready: Option<Arc<Snapshot>>,
}

/// An interrupted/failed/panicking build leaves no ready marker behind.
struct Building<'a>(&'a Slot);
impl Drop for Building<'_> {
    fn drop(&mut self) {
        self.0.state.lock().building = false;
        self.0.changed.notify_all();
    }
}

impl Slot {
    fn load(&self, conn: &Connection, library: &str) -> Result<Arc<Snapshot>, StoreError> {
        loop {
            let mut state = self.state.lock();
            if let Some(ready) = &state.ready {
                return Ok(ready.clone());
            }
            if !state.building {
                state.building = true;
                drop(state);
                let building = Building(self);
                // No registry/slot/writer lock spans this reader snapshot work.
                let ready = Arc::new(Snapshot::read(conn, library)?);
                self.state.lock().ready = Some(ready.clone());
                drop(building);
                return Ok(ready);
            }
            if let Some(budget) = active_query_budget()
                && let Err(error) = budget.check()
            {
                return Err(match error {
                    crate::LibraryReadError::Cancelled => StoreError::ReadCancelled,
                    _ => StoreError::ReadDeadline,
                });
            }
            self.changed.wait_for(&mut state, Duration::from_millis(25));
        }
    }
}

struct Snapshot(BTreeMap<i64, Arc<Document>>);

impl Snapshot {
    fn read(conn: &Connection, library: &str) -> Result<Self, StoreError> {
        let mut documents = BTreeMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT search_rowid,media_id FROM library_items WHERE library_id=?1",
        )?;
        let rows = stmt.query_map([library], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (rowid, media) = row?;
            documents.insert(
                rowid,
                Document {
                    rowid,
                    media,
                    exists: false,
                    tokens: Vec::new(),
                },
            );
        }
        // CROSS JOIN fixes one vocabulary pass followed by integer-rowid seeks.
        let mut stmt = conn.prepare_cached("SELECT v.doc,v.term,v.col,v.offset FROM temp.cm_search_actual_vocab v CROSS JOIN library_items i ON i.search_rowid=v.doc WHERE i.library_id=?1")?;
        let rows = stmt.query_map([library], |r| {
            let token: Token = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok((r.get::<_, i64>(0)?, token))
        })?;
        for row in rows {
            let (rowid, token) = row?;
            if let Some(document) = documents.get_mut(&rowid) {
                document.tokens.push(token);
            }
        }
        let mut stmt = conn.prepare_cached("SELECT f.rowid FROM library_search_fts f CROSS JOIN library_items i ON i.search_rowid=f.rowid WHERE i.library_id=?1")?;
        for row in stmt.query_map([library], |r| r.get::<_, i64>(0))? {
            if let Some(document) = documents.get_mut(&row?) {
                document.exists = true;
            }
        }
        Ok(Self(
            documents
                .into_iter()
                .map(|(rowid, mut doc)| {
                    doc.tokens.sort_unstable();
                    (rowid, Arc::new(doc))
                })
                .collect(),
        ))
    }
}
