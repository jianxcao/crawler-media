//! SQL on one explicitly borrowed database connection.
use super::*;

const QUEUED_DELETION_COLS: &str = "path, file_id, library_id, file_size, file_inode, path_key, deletion_id, scope, file_device, file_mtime_ns, require_target_path, require_target_size, require_target_device, require_target_inode, require_target_mtime_ns, companion_file_id";

fn read_queued_deletion(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedFileDeletion> {
    let scope_raw: String = row.get(7)?;
    let require_target = match (
        row.get::<_, Option<String>>(10)?,
        row.get::<_, Option<i64>>(11)?,
        row.get::<_, Option<i64>>(12)?,
        row.get::<_, Option<i64>>(13)?,
        row.get::<_, Option<i64>>(14)?,
    ) {
        (Some(path), Some(size), Some(device), Some(inode), Some(mtime_ns)) => {
            Some(RequiredDeletionTarget {
                path,
                size_bytes: size as u64,
                device: device as u64,
                inode: inode as u64,
                mtime_ns,
            })
        }
        _ => None,
    };
    Ok(QueuedFileDeletion {
        path: row.get(0)?,
        file_id: row.get(1)?,
        library_id: row.get(2)?,
        file_size: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        file_inode: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
        path_key: row.get(5)?,
        deletion_id: row.get(6)?,
        scope: FileDeletionScope::from_db(&scope_raw)?,
        file_device: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        file_mtime_ns: row.get(9)?,
        require_target,
        companion_file_id: row.get(15)?,
    })
}

pub(crate) struct EnqueueFileDeletion<'a> {
    pub path: &'a str,
    pub reason: &'a str,
    pub scope: FileDeletionScope,
    pub file_size: Option<u64>,
    pub file_device: Option<u64>,
    pub file_inode: Option<u64>,
    pub file_mtime_ns: Option<i64>,
    pub owner: Option<domain::LibraryId>,
    pub require_target: Option<(
        &'a std::path::Path,
        &'a crate::library_write::SidecarSourceIdentity,
    )>,
    pub companion_file_id: Option<&'a str>,
}

impl crate::LibraryConnection<'_> {
    pub fn file_deletion_blocked(&self, path_key: &str) -> Result<bool, StoreError> {
        let key = crate::path_key::canonical_recorded_key(self.library, path_key);
        if let Some(library) = super::scope::owner(self.library, &key)? {
            return match crate::library_write::publish::ensure_library_writable(
                self.library,
                &library,
            ) {
                Ok(()) => Ok(false),
                Err(StoreError::Protected(_) | StoreError::Missing(_)) => Ok(true),
                Err(error) => Err(error),
            };
        }
        self.path_key_in_removal_scope(std::path::Path::new(&key))
    }

    pub fn due_file_deletion(
        &self,
        path: &str,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        let sql = format!(
            "SELECT {QUEUED_DELETION_COLS} FROM pending_file_deletions
             WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1))
               AND next_retry_at<=?2 AND attempts<?3
               AND NOT EXISTS (
                   SELECT 1 FROM library_removals r
                   WHERE r.library_id=pending_file_deletions.library_id)"
        );
        Ok(self
            .library
            .query_row(
                &sql,
                params![path, now, MAX_FILE_DELETION_ATTEMPTS],
                read_queued_deletion,
            )
            .optional()?)
    }

    pub fn file_deletion_pending(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_deletions
                WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1)))",
            [path],
            |row| row.get(0),
        )?)
    }

    /// Accepted deletion intents cannot serve as retained copies in a cleanup plan.
    pub fn file_ids_pending_deletion_in_library(
        &self,
        library: domain::LibraryId,
        media: domain::MediaId,
    ) -> Result<std::collections::HashSet<domain::LedgerId>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT f.file_id FROM library_files f
             JOIN pending_file_deletions d ON d.path_key = f.path_key
             WHERE f.library_id = ?1 AND f.media_id = ?2",
        )?;
        let rows = stmt.query_map(params![library.to_string(), media.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|id| Ok(id?.parse()?)).collect()
    }

    pub fn enqueue_file_deletion_observed(
        &self,
        path: &str,
        reason: &str,
        identity: Option<(u64, Option<u64>)>,
    ) -> Result<(), StoreError> {
        self.enqueue_file_deletion(EnqueueFileDeletion {
            path,
            reason,
            scope: FileDeletionScope::MediaWithLeftovers,
            file_size: identity.map(|(size, _)| size),
            file_device: None,
            file_inode: identity.and_then(|(_, inode)| inode),
            file_mtime_ns: None,
            owner: None,
            require_target: None,
            companion_file_id: None,
        })
    }

    pub(crate) fn enqueue_file_deletion(
        &self,
        req: EnqueueFileDeletion<'_>,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let key = crate::path_key::canonical_recorded_key(&tx, req.path);
        let file_info: Option<(String, String)> = tx
            .query_row(
                "SELECT file_id,library_id FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [req.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (file_id, library_id) = if let Some((file_id, library_id)) = file_info {
            if req
                .owner
                .is_some_and(|owner| owner.to_string() != library_id)
            {
                return Err(StoreError::Protected(
                    "physical deletion owner changed".into(),
                ));
            }
            crate::library_write::publish::ensure_library_writable(&tx, &library_id)?;
            let placing: bool = tx.query_row(
                "SELECT publication_state='pending' FROM library_files WHERE file_id=?1",
                [&file_id],
                |row| row.get(0),
            )?;
            let renaming: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE file_id=?1 AND phase <> 'failed')",
                [&file_id],
                |row| row.get(0),
            )?;
            if renaming || placing {
                tracing::warn!(
                    file_id,
                    renaming,
                    placing,
                    "拒绝删除仍有文件发布所有者的路径"
                );
                return Err(StoreError::Protected(format!(
                    "file {file_id} has a rename or placement owner"
                )));
            }
            (Some(file_id), Some(library_id))
        } else {
            let library = req
                .owner
                .map(|id| id.to_string())
                .or(super::scope::owner(&tx, &key)?);
            if let Some(library) = &library {
                crate::library_write::publish::ensure_library_writable(&tx, library)?;
            } else if self.file_deletion_blocked(&key)? {
                return Err(StoreError::Protected(format!(
                    "Library refuses physical deletion of {}",
                    req.path
                )));
            }
            (None, library)
        };
        let (req_path, req_size, req_dev, req_ino, req_mtime) = match req.require_target {
            Some((path, id)) => (
                Some(path.to_string_lossy().into_owned()),
                Some(id.size_bytes as i64),
                Some(id.device as i64),
                Some(id.inode as i64),
                Some(id.mtime_ns),
            ),
            None => (None, None, None, None, None),
        };
        tx.execute(
            "INSERT INTO pending_file_deletions (
                path, path_key, reason, scope, file_id, library_id,
                file_size, file_device, file_inode, file_mtime_ns,
                require_target_path, require_target_size, require_target_device,
                require_target_inode, require_target_mtime_ns, companion_file_id,
                deletion_id
             )
             VALUES (?1, norm_path_key(?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT DO NOTHING",
            params![
                req.path,
                req.reason,
                req.scope.as_str(),
                file_id,
                library_id,
                req.file_size.map(|v| v as i64),
                req.file_device.map(|v| v as i64),
                req.file_inode.map(|v| v as i64),
                req.file_mtime_ns,
                req_path,
                req_size,
                req_dev,
                req_ino,
                req_mtime,
                req.companion_file_id,
                uuid::Uuid::new_v4().to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_due_file_deletions(&self, now: i64) -> Result<Vec<QueuedFileDeletion>, StoreError> {
        let sql = format!(
            "SELECT {QUEUED_DELETION_COLS} FROM pending_file_deletions
             WHERE next_retry_at <= ?1 AND attempts < ?2
             AND NOT EXISTS (
                   SELECT 1 FROM library_removals r
                   WHERE r.library_id=pending_file_deletions.library_id) ORDER BY next_retry_at"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            params![now, MAX_FILE_DELETION_ATTEMPTS],
            read_queued_deletion,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn due_file_deletion_intent(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        let sql = format!(
            "SELECT {QUEUED_DELETION_COLS} FROM pending_file_deletions
             WHERE path_key=?1 AND deletion_id=?2 AND next_retry_at<=?3 AND attempts<?4
             AND NOT EXISTS (
                   SELECT 1 FROM library_removals r
                   WHERE r.library_id=pending_file_deletions.library_id)"
        );
        Ok(self
            .library
            .query_row(
                &sql,
                params![
                    entry.path_key,
                    entry.deletion_id,
                    now,
                    MAX_FILE_DELETION_ATTEMPTS
                ],
                read_queued_deletion,
            )
            .optional()?)
    }

    pub fn file_deletion_intent_pending(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<bool, StoreError> {
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2)",
            params![entry.path_key,entry.deletion_id], |row| row.get(0),
        )?)
    }

    pub fn file_id_at_deletion_path(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT file_id FROM library_files WHERE path_key=?1",
                [&entry.path_key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn abandon_file_deletion(&self, entry: &QueuedFileDeletion) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        Ok(())
    }

    pub fn abandon_file_deletion_restoring_sidecar(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let deleted = tx.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        if deleted > 0
            && entry.scope == FileDeletionScope::ExactPath
            && let (Some(file_id), Some(target)) = (&entry.companion_file_id, &entry.require_target)
        {
            let target_key = crate::path_key::canonical_recorded_key(&tx, &target.path);
            let source_key = crate::path_key::canonical_recorded_key(&tx, &entry.path);
            tx.execute(
                "UPDATE scrape_sidecars SET path = ?3, path_key = ?4
                 WHERE file_id = ?1 AND path_key = ?2",
                params![file_id, target_key, entry.path, source_key],
            )?;
            tx.execute(
                "UPDATE library_files SET
                    poster_path = CASE WHEN poster_path_key = ?2 THEN ?3 ELSE poster_path END,
                    poster_path_key = CASE WHEN poster_path_key = ?2 THEN ?4 ELSE poster_path_key END,
                    backdrop_path = CASE WHEN backdrop_path_key = ?2 THEN ?3 ELSE backdrop_path END,
                    backdrop_path_key = CASE WHEN backdrop_path_key = ?2 THEN ?4 ELSE backdrop_path_key END,
                    still_path = CASE WHEN still_path_key = ?2 THEN ?3 ELSE still_path END,
                    still_path_key = CASE WHEN still_path_key = ?2 THEN ?4 ELSE still_path_key END
                 WHERE file_id = ?1",
                params![file_id, target_key, entry.path, source_key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_stuck_file_deletions(&self) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, last_error FROM pending_file_deletions
             WHERE attempts >= ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn reset_file_deletion_backoff(&self, path: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions SET next_retry_at = 0, attempts = 0
             WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1))",
            [path],
        )?;
        Ok(())
    }

    pub fn file_id_at_path(&self, path: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT file_id FROM library_files WHERE path_key = norm_path_key(?1) LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn list_pending_file_deletions(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path FROM pending_file_deletions ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_file_deletion_failure(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions
             SET attempts = attempts + 1,
                 next_retry_at = ?3 + MIN(86400, 60 * (1 << MIN(attempts, 10))),
                 last_error = ?4
             WHERE path_key = ?1 AND deletion_id = ?2",
            params![entry.path_key, entry.deletion_id, now, error],
        )?;
        Ok(())
    }
}

impl crate::LibraryConnection<'_> {
    pub fn complete_file_deletion_with_references(
        &self,
        entry: &QueuedFileDeletion,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let queued_file_id: Option<Option<String>> = tx
            .query_row(
                "SELECT file_id FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
                params![entry.path_key, entry.deletion_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(queued_file_id) = queued_file_id else {
            return Ok(());
        };
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT file_id,library_id FROM library_files WHERE path_key=?1",
                [&entry.path_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((file_id, library_id)) = current
            && Some(file_id.as_str()) == queued_file_id.as_deref()
        {
            let (_, affected) = crate::library_write::delete::delete_files_in_tx(
                &tx,
                library_id.parse()?,
                &[file_id.parse()?],
            )?;
            crate::library_write::orphan_media::purge_orphan_media_in_tx(&tx, &affected)?;
        }
        tx.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
