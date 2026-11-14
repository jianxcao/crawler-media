//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn list_library_roots(&self) -> Result<Vec<LibraryRoot>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT r.root_id, l.kind, r.display_path,
                    (l.is_default = 1 AND r.sort_order = 0) AS primary_root,
                    r.library_id, r.sort_order, r.enabled
             FROM library_roots r
             JOIN libraries l ON r.library_id = l.library_id
             ORDER BY l.is_default DESC, r.sort_order, r.display_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, String>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, i64>(6)? != 0,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (id, kind, path, is_default, library_id, sort_order, enabled) = row?;
            roots.push(LibraryRoot {
                id,
                kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                path: PathBuf::from(path),
                is_default,
                library_id,
                sort_order,
                enabled,
            });
        }
        Ok(roots)
    }

    pub fn get_library_root(&self, id: &str) -> Result<Option<LibraryRoot>, StoreError> {
        Ok(self
            .list_library_roots()?
            .into_iter()
            .find(|root| root.id == id))
    }

    pub fn delete_extra_library_root(&self, id: &str) -> Result<bool, StoreError> {
        let root = match self.get_library_root(id)? {
            Some(r) => r,
            None => return Ok(false),
        };
        if root.is_default {
            return Ok(false);
        }
        self.ensure_library_mutable(&root.library_id)?;
        let n = self
            .library
            .execute("DELETE FROM library_roots WHERE root_id = ?1", params![id])?;
        crate::path_key::reload_root_aliases(self.library)?;
        Ok(n > 0)
    }

    pub fn drain_root_reassignments(&self) -> Result<usize, StoreError> {
        let mut batches = 0;
        loop {
            if !pending_root_work(self)? {
                break;
            }
            match self.drain_one_root_batch()? {
                Some(true) => batches += 1,
                _ => break,
            }
        }
        Ok(batches)
    }

    pub fn drain_one_root_batch(&self) -> Result<Option<bool>, StoreError> {
        let pending: Vec<(String, String)> = {
            let mut statement = self.library.prepare(
                "SELECT kind,root_key FROM pending_root_reassignments
                 ORDER BY dispatch_order,kind,root_key LIMIT 1",
            )?;
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        if pending.is_empty() {
            return Ok(Some(false));
        }
        for (kind, root_key) in pending {
            if !take_batch_budget() {
                return Ok(None);
            }
            let parsed: MediaKind = kind
                .parse()
                .map_err(|_| StoreError::Invalid(format!("reassignment kind {kind}")))?;
            let started = std::time::Instant::now();
            let tx = crate::write_tx(self.library)?;
            // Library creation can own the surrounding transaction. A rejected
            // window must undo only its own moves, retaining that outer work.
            tx.execute_batch("SAVEPOINT root_reassignment_batch")?;
            let window = (|| {
                let done = reassign_root_batch(&tx, parsed, &root_key)?;
                let state = if done == Some(true) {
                    "succeeded"
                } else if done.is_some() {
                    "running"
                } else {
                    "blocked"
                };
                status::update(&tx, parsed, &root_key, state, None)?;
                rotate_root(&tx, &kind, &root_key)?;
                Ok::<_, StoreError>((done, state))
            })();
            let (done, state) = match window {
                Ok(window) => window,
                Err(error) => {
                    tx.execute_batch(
                        "ROLLBACK TO root_reassignment_batch; RELEASE root_reassignment_batch",
                    )?;
                    status::update(&tx, parsed, &root_key, "failed", Some(&error.to_string()))?;
                    rotate_root(&tx, &kind, &root_key)?;
                    tx.commit()?;
                    tracing::error!(kind,root_key,%error,"Root reassignment window rejected; retained for retry");
                    return Err(error);
                }
            };
            tx.execute_batch("RELEASE root_reassignment_batch")?;
            tx.commit()?;
            if done.is_some() {
                tracing::debug!(
                    kind,
                    root_key,
                    elapsed_ms = started.elapsed().as_millis(),
                    state,
                    "Root reassignment window committed"
                );
                return Ok(Some(true));
            }
            tracing::debug!(
                kind,
                root_key,
                "Root reassignment paused; rotating other roots"
            );
        }
        Ok(None)
    }
}

impl crate::LibraryConnection<'_> {
    pub fn insert_library_root_prepared(
        &self,
        kind: MediaKind,
        path: &str,
        key: &str,
    ) -> Result<LibraryRoot, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let library = self
            .default_library(kind)?
            .ok_or_else(|| StoreError::Missing(format!("default {} library", kind.as_str())))?;
        self.ensure_library_mutable(&library.id)?;

        self.ensure_removal_keys_available([key])?;
        let sort_order = self.roots_of(&library.id)?.len() as i32;
        let tx = crate::write_tx(self.library)?;
        tx.execute(
            "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
             VALUES (?1, ?2, ?5, ?3, 1, ?4)",
            params![
                id,
                library.id,
                path,
                sort_order,
                key
            ],
        )?;
        Store::reassign_files_under_root(&tx, &library.id, kind, key)?;
        tx.commit()?;
        crate::path_key::reload_root_aliases(self.library)?;
        let _ = self.drain_one_root_batch()?;
        Ok(LibraryRoot {
            id,
            kind,
            path: PathBuf::from(path),
            is_default: false,
            library_id: library.id,
            sort_order,
            enabled: true,
        })
    }
}
