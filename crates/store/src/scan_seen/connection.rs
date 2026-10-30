//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn record_scan_seen_stamps(
        &self,
        operation_id: &str,
        stamps: &[(String, i64, i64, Option<String>)],
    ) -> Result<(), StoreError> {
        if stamps.is_empty() {
            return Ok(());
        }
        let tx = crate::write_tx(self.library)?;
        let aliases = crate::path_key::alias_snapshot(self.library);
        let keys: Vec<String> = stamps
            .iter()
            .map(|(path, _, _, _)| crate::path_key::canonical_key(path, &aliases))
            .collect();
        for ((path, mtime_ms, file_size, content_digest), path_key) in
            stamps.iter().zip(keys.iter())
        {
            tx.execute(
                "INSERT INTO scan_seen (operation_id, path, path_key, mtime_ms, file_size, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(operation_id, path_key)
                 DO UPDATE SET path = excluded.path, mtime_ms = excluded.mtime_ms, file_size = excluded.file_size,
                               content_digest = excluded.content_digest",
                params![operation_id, path, path_key, mtime_ms, file_size, content_digest],
            )?;
            // 只有已登记的台账行才有 durable 源版本；没有台账行的路径这里是空操作。
            tx.execute(
                "UPDATE library_files
                    SET mtime_ns = ?2 * 1000000, size_bytes = ?3,
                        source_digest = ?4
                  WHERE path_key = ?1",
                params![path_key, mtime_ms, file_size, content_digest],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_source_version(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE library_files
                SET mtime_ns = NULL, size_bytes = NULL, source_digest = NULL,
                    created_ns = NULL, stat_known = 0
              WHERE file_id = ?1",
            params![ledger_id],
        )?;
        Ok(())
    }

    pub fn ledger_source_versions(
        &self,
        paths: &[String],
    ) -> Result<HashMap<String, (i64, i64, Option<String>)>, StoreError> {
        let mut versions = HashMap::new();
        if paths.is_empty() {
            return Ok(versions);
        }
        let aliases = crate::path_key::alias_snapshot(self.library);
        // 分块：复用 library_files.path_key 索引，同时不触碰 SQLite 的变量上限。
        for chunk in paths.chunks(500) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT path_key, COALESCE(mtime_ns / 1000000, 0), COALESCE(size_bytes, 0), source_digest
                 FROM library_files WHERE path_key IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.library.prepare(&sql)?;
            let keys: Vec<String> = chunk
                .iter()
                .map(|path| crate::path_key::canonical_key(path, &aliases))
                .collect();
            let rows = stmt.query_map(rusqlite::params_from_iter(keys.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get(3)?),
                ))
            })?;
            let by_key: HashMap<String, (i64, i64, Option<String>)> =
                rows.collect::<Result<_, _>>()?;
            for (path, key) in chunk.iter().zip(keys.iter()) {
                if let Some(version) = by_key.get(key) {
                    versions.insert(path.clone(), version.clone());
                }
            }
        }
        Ok(versions)
    }

    pub fn scan_seen_checkpoints(
        &self,
        operation_id: &str,
        paths: &[String],
    ) -> Result<HashMap<String, (i64, i64, Option<String>, bool)>, StoreError> {
        let mut checkpoints = HashMap::new();
        if paths.is_empty() {
            return Ok(checkpoints);
        }
        let aliases = crate::path_key::alias_snapshot(self.library);
        for chunk in paths.chunks(500) {
            let keys: Vec<String> = chunk
                .iter()
                .map(|path| crate::path_key::canonical_key(path, &aliases))
                .collect();
            let placeholders = (2..=keys.len() + 1)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT s.path_key, s.mtime_ms, s.file_size, s.content_digest,
                        f.file_id IS NOT NULL AND f.missing_at IS NULL
                 FROM scan_seen s LEFT JOIN library_files f ON f.path_key = s.path_key
                 WHERE s.operation_id = ?1 AND s.path_key IN ({placeholders})"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(
                    std::iter::once(operation_id).chain(keys.iter().map(String::as_str)),
                ),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
                    ))
                },
            )?;
            let by_key: HashMap<String, (i64, i64, Option<String>, bool)> =
                rows.collect::<Result<_, _>>()?;
            for (path, key) in chunk.iter().zip(keys.iter()) {
                if let Some(checkpoint) = by_key.get(key) {
                    checkpoints.insert(path.clone(), checkpoint.clone());
                }
            }
        }
        Ok(checkpoints)
    }

    pub fn is_scan_seen(&self, operation_id: &str, path: &str) -> Result<bool, StoreError> {
        let found: bool = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM scan_seen WHERE operation_id = ?1 AND path_key = ?2)",
            params![operation_id, self.recorded_path_key(path)],
            |r| r.get(0),
        )?;
        Ok(found)
    }

    pub fn clear_scan_seen(&self, operation_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM scan_seen WHERE operation_id = ?1",
            [operation_id],
        )?;
        Ok(())
    }
}
