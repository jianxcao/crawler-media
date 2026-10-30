//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn due_file_deletion(
        &self,
        path: &str,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        Ok(self.library.query_row(
            "SELECT path,file_id,file_size,file_inode,path_key,deletion_id FROM pending_file_deletions
             WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1))
               AND next_retry_at<=?2 AND attempts<?3",
            params![path,now,MAX_FILE_DELETION_ATTEMPTS],
            |row| Ok(QueuedFileDeletion {
                path:row.get(0)?, file_id:row.get(1)?,
                file_size:row.get::<_,Option<i64>>(2)?.map(|v| v as u64),
                file_inode:row.get::<_,Option<i64>>(3)?.map(|v| v as u64),
                path_key:row.get(4)?,
                deletion_id:row.get(5)?,
            }),
        ).optional()?)
    }

    pub fn file_deletion_pending(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_file_deletions
                WHERE path_key=COALESCE((SELECT path_key FROM pending_file_deletions WHERE path=?1),norm_path_key(?1)))",
            [path],
            |row| row.get(0),
        )?)
    }

    pub fn enqueue_file_deletion_observed(
        &self,
        path: &str,
        reason: &str,
        identity: Option<(u64, Option<u64>)>,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let file_id: Option<String> = tx
            .query_row(
                "SELECT file_id FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(file_id) = &file_id {
            let placing: bool = tx.query_row(
                "SELECT publication_state='pending' FROM library_files WHERE file_id=?1",
                [file_id],
                |row| row.get(0),
            )?;
            let renaming: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_file_renames WHERE file_id=?1)",
                [file_id],
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
        }
        tx.execute(
            "INSERT INTO pending_file_deletions (path, path_key, reason, file_id, file_size, file_inode, deletion_id)
             VALUES (?1, norm_path_key(?1), ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO NOTHING",
            params![
                path,
                reason,
                file_id,
                identity.map(|(size, _)| size as i64),
                identity.and_then(|(_, ino)| ino).map(|ino| ino as i64),
                uuid::Uuid::new_v4().to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_due_file_deletions(&self, now: i64) -> Result<Vec<QueuedFileDeletion>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, file_id, file_size, file_inode, path_key, deletion_id FROM pending_file_deletions
             WHERE next_retry_at <= ?1 AND attempts < ?2 ORDER BY next_retry_at",
        )?;
        let rows = stmt.query_map(params![now, MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok(QueuedFileDeletion {
                path: row.get(0)?,
                path_key: row.get(4)?,
                deletion_id: row.get(5)?,
                file_id: row.get(1)?,
                file_size: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                file_inode: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn due_file_deletion_intent(
        &self,
        entry: &QueuedFileDeletion,
        now: i64,
    ) -> Result<Option<QueuedFileDeletion>, StoreError> {
        Ok(self.library.query_row(
            "SELECT path,file_id,file_size,file_inode,path_key,deletion_id FROM pending_file_deletions
             WHERE path_key=?1 AND deletion_id=?2 AND next_retry_at<=?3 AND attempts<?4",
            params![entry.path_key,entry.deletion_id,now,MAX_FILE_DELETION_ATTEMPTS],
            |row| Ok(QueuedFileDeletion {
                path:row.get(0)?, file_id:row.get(1)?,
                file_size:row.get::<_,Option<i64>>(2)?.map(|v|v as u64),
                file_inode:row.get::<_,Option<i64>>(3)?.map(|v|v as u64),
                path_key:row.get(4)?, deletion_id:row.get(5)?,
            }),
        ).optional()?)
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
        subscribes: &crate::SubscribeConnection<'_>,
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
            let subscribed = subscribes.subscribed_media(&affected)?;
            crate::library_write::orphan_media::purge_orphan_media_in_tx(
                &tx,
                &affected,
                &subscribed,
            )?;
        }
        tx.execute(
            "DELETE FROM pending_file_deletions WHERE path_key=?1 AND deletion_id=?2",
            params![entry.path_key, entry.deletion_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
