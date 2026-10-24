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
        let key = crate::path_key::canonical_recorded_key(&self.library, &path);
        let tx = crate::write_tx(&self.library)?;
        let scope: Option<(String, String)> = tx
            .query_row(
                "SELECT library_id,root_id FROM library_roots WHERE enabled=1 AND
             (path_key='/' OR ?1=path_key OR substr(?1,1,length(path_key)+1)=path_key||'/')
             ORDER BY length(path_key) DESC LIMIT 1",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // Intake outside all roots parks in the default Movie Library. This is
        // an explicit intake policy; reads and claims retain this recorded identity.
        let (library, root) = match scope {
            Some(scope) => scope,
            None => tx.query_row(
                "SELECT l.library_id,r.root_id FROM libraries l JOIN library_roots r USING(library_id)
                 WHERE l.kind='movie' AND l.is_default=1 AND r.sort_order=0 AND r.enabled=1",
                [], |r| Ok((r.get(0)?,r.get(1)?)),
            ).optional()?.ok_or_else(|| StoreError::Missing("default Movie Library root".into()))?,
        };
        super::library_write::publish::ensure_library_writable(&tx, &library)?;
        tx.execute(
            "INSERT INTO unidentified(library_id,root_id,path,path_key,confidence,unidentified_id) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(library_id,path_key) DO UPDATE SET root_id=excluded.root_id,
               path=excluded.path,confidence=excluded.confidence,revision=unidentified.revision+1",
            params![library,root,path,key,confidence.as_str(),uuid::Uuid::new_v4().to_string()],
        )?;
        // New facts already name the current deepest root. Only a misowned
        // duplicate before the cursor requires a rewind; ordinary intake must
        // not keep replaying completed windows and starve the remaining tail.
        tx.execute(
            "UPDATE pending_root_reassignments SET unidentified_cursor_key='',unidentified_cursor_id=''
             WHERE (root_key='/' OR ?1=root_key OR substr(?1,1,length(root_key)+1)=root_key||'/')
             AND unidentified_cursor_key>=?1 AND EXISTS(SELECT 1 FROM unidentified
             WHERE path_key=?1 AND (library_id<>?2 OR root_id<>?3))",
            params![key,library,root],
        )?;
        tx.execute(
            "UPDATE root_reassignment_state SET state='queued',revision=revision+1,error=NULL,
             updated_at_ms=?1 WHERE EXISTS(SELECT 1 FROM pending_root_reassignments p
             WHERE p.kind=root_reassignment_state.kind AND p.root_key=root_reassignment_state.root_key)
             AND (root_key='/' OR ?2=root_key OR substr(?2,1,length(root_key)+1)=root_key||'/')",
            params![crate::unix_now()*1000,key],
        )?;
        tx.commit()?;
        tracing::info!(
            library_id = library,
            root_id = root,
            path,
            "Unidentified parked"
        );
        Ok(())
    }

    pub fn list_unidentified(
        &self,
        library_id: Option<&str>,
    ) -> Result<Vec<UnidentifiedRow>, StoreError> {
        let sql = if library_id.is_some() {
            format!("{SELECT} WHERE u.library_id=?1 ORDER BY u.path_key")
        } else {
            format!("{SELECT} ORDER BY u.path_key,u.library_id")
        };
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(library_id), map_row)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn get_unidentified(&self, path: &str) -> Result<Option<UnidentifiedRow>, StoreError> {
        let key = crate::path_key::canonical_recorded_key(&self.library, path);
        let mut stmt = self
            .library
            .prepare(&format!("{SELECT} WHERE u.path_key=?1 LIMIT 2"))?;
        let mut rows = stmt.query_map([key], map_row)?;
        let first = rows.next().transpose()?;
        if rows.next().is_some() {
            return Err(StoreError::Protected(
                "ambiguous Unidentified identity".into(),
            ));
        }
        Ok(first)
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
