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
        state(&self.library)
    }

    pub fn change_metadata_language(
        &self,
        expected_revision: i64,
        language: &str,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<MetadataLanguageState, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let current = state(&tx)?;
        if current.revision != expected_revision {
            return Err(StoreError::Protected(
                "metadata language revision changed".into(),
            ));
        }
        let language = language.trim();
        if language == current.language {
            return Ok(current);
        }
        let libraries = {
            let mut stmt = tx.prepare("SELECT library_id,lifecycle_state,config_revision FROM libraries ORDER BY library_id")?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (library, lifecycle, _) in &libraries {
            self.ensure_library_mutable(library)?;
            crate::library_operations::integrity_guard::ensure_no_operation(&tx, library.parse()?)?;
            if lifecycle != "ready" {
                return Err(StoreError::Protected(format!(
                    "library {library} lifecycle is {lifecycle}"
                )));
            }
        }
        let revision = current.revision + 1;
        tx.execute(
            "UPDATE library_query_config SET default_locale=NULLIF(?1,''),revision=?2 WHERE id=1",
            params![language, revision],
        )?;
        for (library, _, config_revision) in libraries {
            let child = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("default-locale:{operation_id}:{library}").as_bytes(),
            )
            .to_string();
            let plan = serde_json::json!({"default_config_revision":revision});
            tx.execute("INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,plan_json,created_at_ms,updated_at_ms) VALUES (?1,?2,'default_locale','frozen',?3,?4,?5,?5)",params![child,library,config_revision,plan.to_string(),now_ms])?;
            tx.execute("UPDATE libraries SET lifecycle_state='reindexing',config_revision=config_revision+1 WHERE library_id=?1",[library])?;
        }
        let result = state(&tx)?;
        tx.commit()?;
        tracing::info!(
            revision,
            language,
            libraries = result.operations.len(),
            "metadata language rebuild registered"
        );
        Ok(result)
    }
}
