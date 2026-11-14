//! Instance metadata language and durable, atomic Library rebuild registration.
use crate::{Store, StoreError};
use domain::LibraryId;
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct MetadataLanguageOperation {
    pub library_id: LibraryId,
    pub operation_id: String,
    pub state: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct MetadataLanguageState {
    pub language: String,
    pub revision: i64,
    pub operations: Vec<MetadataLanguageOperation>,
}

pub(crate) fn default_locale(db: &Connection) -> Result<Option<String>, StoreError> {
    Ok(db.query_row(
        "SELECT default_locale FROM library_query_config WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
fn state(db: &Connection) -> Result<MetadataLanguageState, StoreError> {
    let (language, revision) = db.query_row(
        "SELECT COALESCE(default_locale,''),revision FROM library_query_config WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)?)),
    )?;
    let mut stmt = db.prepare("SELECT library_id,operation_id,state,error FROM library_operations WHERE kind='default_locale' AND json_extract(plan_json,'$.default_config_revision')=?1 ORDER BY library_id")?;
    let rows = stmt
        .query_map([revision], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let operations = rows
        .into_iter()
        .map(|(library, operation_id, state, error)| {
            Ok(MetadataLanguageOperation {
                library_id: library.parse()?,
                operation_id,
                state,
                error,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(MetadataLanguageState {
        language,
        revision,
        operations,
    })
}
impl Store {
    pub fn metadata_language(&self) -> Result<MetadataLanguageState, StoreError> {
        self.with_library_read(|db| db.metadata_language())
    }

    pub fn change_metadata_language(
        &self,
        expected_revision: i64,
        language: &str,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<MetadataLanguageState, StoreError> {
        self.with_library_write(|db| {
            db.change_metadata_language(expected_revision, language, operation_id, now_ms)
        })
    }
}

#[path = "metadata_language/connection.rs"]
mod connection;
