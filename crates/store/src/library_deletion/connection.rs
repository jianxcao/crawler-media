//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn path_key_in_removal_scope(&self, path: &Path) -> Result<bool, StoreError> {
        let mut statement = self
            .library
            .prepare("SELECT path FROM library_removal_roots")?;
        for root in statement.query_map([], |row| row.get::<_, String>(0))? {
            if path.starts_with(Path::new(&root?)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn ensure_library_mutable(&self, id: &str) -> Result<(), StoreError> {
        if let Some(removal) = self.removal_for_library(id)?
            && removal.state != SUCCEEDED_STATE
        {
            tracing::error!(library_id = id, operation_id = %removal.operation_id,
                    "拒绝修改冻结的 Library");
            return Err(StoreError::Protected(format!("Library {id} 已冻结")));
        }
        Ok(())
    }

    pub(crate) fn ensure_removal_keys_available<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), StoreError> {
        let proposed = keys.into_iter().map(Path::new).collect::<Vec<_>>();
        for removal in self.pending_library_removals()? {
            for root in self.removal_roots(&removal.operation_id)? {
                if proposed
                    .iter()
                    .any(|path| path.starts_with(&root) || root.starts_with(path))
                {
                    tracing::error!(operation_id = %removal.operation_id, root = %root.display(),
                        "拒绝与待删除 Library 重叠的根目录");
                    return Err(StoreError::Protected("根目录与待删除 Library 重叠".into()));
                }
            }
        }
        Ok(())
    }

    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let existing = tx
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
                [id],
                map_removal,
            )
            .optional()?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
        crate::library_operations::integrity_guard::ensure_no_operation(&tx, id.parse()?)?;
        let (kind, revision): (String, i64) = tx
            .query_row(
                "SELECT kind,config_revision FROM libraries WHERE library_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM libraries l WHERE kind=?1 AND NOT EXISTS
             (SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id)",
            [&kind],
            |row| row.get(0),
        )?;
        if count <= 1 {
            return Err(StoreError::Protected("每种类型的媒体库至少保留一个".into()));
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO library_removals(operation_id,library_id,state,config_revision,
             created_at_ms,updated_at_ms) VALUES(?1,?2,'frozen',?3,?4,?4)",
            params![operation_id, id, revision, crate::unix_now() * 1000],
        )?;
        tx.execute(
            "INSERT INTO library_removal_roots(operation_id,path)
             SELECT ?1,path_key FROM library_roots WHERE library_id=?2",
            params![operation_id, id],
        )?;
        snapshot::capture_batch(&tx, &operation_id)?;
        tx.commit()?;
        tracing::info!(library_id=id,%operation_id,"Library 删除已冻结");
        self.library_removal(&operation_id)?
            .ok_or_else(|| StoreError::Missing(operation_id))
    }

    pub fn removal_for_library(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    pub fn library_removal(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE operation_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state != '{SUCCEEDED_STATE}'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn removal_roots(&self, id: &str) -> Result<Vec<PathBuf>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT path FROM library_removal_roots WHERE operation_id = ?1 ORDER BY path",
        )?;
        Ok(statement
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    pub fn succeeded_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .library
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state = '{SUCCEEDED_STATE}'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.get_library(id)?.is_some() && self.removal_for_library(id)?.is_none())
    }

    pub fn capture_library_removal_batch(&self, id: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let done = snapshot::capture_batch(&tx, id)?;
        tx.commit()?;
        Ok(done)
    }

    pub fn removal_ledger_ids_after(
        &self,
        id: &str,
        after: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT file_id FROM library_removal_files WHERE operation_id=?1 AND file_id>?2
             ORDER BY file_id LIMIT 100",
        )?;
        Ok(statement
            .query_map(params![id, after], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub fn set_removal_state(
        &self,
        id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        if !matches!(state, "frozen" | "draining" | "cleaning" | "failed") {
            return Err(StoreError::Protected(format!(
                "invalid removal state {state}"
            )));
        }
        let changed = self.library.execute(
            "UPDATE library_removals SET state=?2, error=?3, updated_at_ms=?4
             WHERE operation_id=?1 AND state != 'succeeded'",
            params![id, state, error, crate::unix_now() * 1000],
        )?;
        if changed == 0 {
            return Err(StoreError::Missing(format!("active removal {id}")));
        }
        Ok(())
    }
}
