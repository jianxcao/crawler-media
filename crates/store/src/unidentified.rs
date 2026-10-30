//! Parked file identity is persisted, rather than inferred again by readers.
use domain::Confidence;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnidentifiedRow {
    pub unidentified_id: String,
    pub library_id: String,
    pub root_id: String,
    pub path: String,
    pub path_key: String,
    pub confidence: String,
    pub revision: i64,
    pub config_revision: i64,
    pub library_name: String,
}

const SELECT: &str = "SELECT u.library_id,u.root_id,u.path,u.path_key,u.confidence,u.revision,
    l.config_revision,l.name,u.unidentified_id FROM unidentified u JOIN libraries l USING(library_id)";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UnidentifiedRow> {
    Ok(UnidentifiedRow {
        library_id: row.get(0)?,
        root_id: row.get(1)?,
        path: row.get(2)?,
        path_key: row.get(3)?,
        confidence: row.get(4)?,
        revision: row.get(5)?,
        config_revision: row.get(6)?,
        library_name: row.get(7)?,
        unidentified_id: row.get(8)?,
    })
}

impl Store {
    pub fn insert_unidentified(
        &self,
        path: String,
        confidence: Confidence,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.insert_unidentified(path, confidence))
    }

    pub fn list_unidentified(
        &self,
        library_id: Option<&str>,
    ) -> Result<Vec<UnidentifiedRow>, StoreError> {
        self.with_library_read(|db| db.list_unidentified(library_id))
    }

    pub fn get_unidentified(&self, path: &str) -> Result<Option<UnidentifiedRow>, StoreError> {
        self.with_library_read(|db| db.get_unidentified(path))
    }
}

/// Retire exactly the observed source, in the same transaction as publication.
pub(crate) fn consume(tx: &Connection, source: &UnidentifiedRow) -> Result<(), StoreError> {
    super::library_write::publish::ensure_library_writable(tx, &source.library_id)?;
    let changed = tx.execute(
        "DELETE FROM unidentified WHERE library_id=?1 AND root_id=?2 AND path_key=?3 AND revision=?4 AND unidentified_id=?6
         AND EXISTS(SELECT 1 FROM libraries WHERE library_id=?1 AND config_revision=?5)",
        params![source.library_id,source.root_id,source.path_key,source.revision,source.config_revision,source.unidentified_id],
    )?;
    if changed != 1 {
        return Err(StoreError::Protected(
            "Unidentified source identity changed".into(),
        ));
    }
    Ok(())
}

#[path = "unidentified/connection.rs"]
mod connection;
