//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_read_snapshot<T>(&self, read: impl FnOnce(&Self) -> T) -> Result<T, StoreError> {
        if !self.library.is_autocommit() {
            return Ok(read(self));
        }
        let tx = crate::read_tx(self.library)?;
        let value = read(self);
        tx.commit()?;
        Ok(value)
    }

    pub fn has_file_in_library(
        &self,
        library: LibraryId,
        media: MediaId,
    ) -> Result<bool, StoreError> {
        self.library
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM library_files WHERE library_id=?1 AND media_id=?2)",
                params![library.to_string(), media.to_string()],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn file_coverage_in_library_season(
        &self,
        library: LibraryId,
        media: MediaId,
        season: u32,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        let sql = format!(
            "SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode
            FROM library_file_units u JOIN library_files f ON f.file_id=u.file_id
            WHERE u.library_id=?1 AND u.media_id=?2 AND u.season=?3"
        );
        self.read_coverage(
            &sql,
            params![library.to_string(), media.to_string(), season as i64],
        )
    }

    pub(super) fn read_coverage<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        let mut stmt = self.library.prepare(sql)?;
        let rows = stmt
            .query_map(params, map_file_unit)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(group_file_coverage(rows))
    }

    pub fn file_coverage_by_path(&self, path: &str) -> Result<Option<FileCoverage>, StoreError> {
        let sql = coverage_sql(
            "WHERE f.file_id = (SELECT file_id FROM library_files
                                WHERE path_key = norm_path_key(?1) LIMIT 1)",
        );
        Ok(self
            .read_coverage(&sql, [stored_path_key(path)])?
            .into_iter()
            .next())
    }

    pub fn file_version_for_path(
        &self,
        path: &str,
    ) -> Result<Option<(LedgerId, FileVersion)>, StoreError> {
        let key = stored_path_key(path);
        self.library
            .query_row(
                "SELECT file_id, file_revision, source_version FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [key],
                |row| {
                    let id: String = row.get(0)?;
                    Ok((
                        id.parse::<LedgerId>()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                        FileVersion {
                            revision: row.get(1)?,
                            source_version: row.get(2)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_file_units(&self, file_id: LedgerId) -> Result<Vec<UnitKey>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT season, episode FROM library_file_units WHERE file_id = ?1 ORDER BY season ASC, episode ASC",
        )?;
        let rows = stmt.query_map(params![file_id.to_string()], |row| {
            Ok(UnitKey::new(row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn file_coverage_for_media(
        &self,
        media_id: MediaId,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.media_ledger_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sql = coverage_sql("WHERE f.media_id = ?1");
        self.read_coverage(&sql, [media_id.to_string()])
    }

    pub fn file_coverage_for_media_batch(
        &self,
        ids: &[MediaId],
    ) -> Result<Vec<FileCoverage>, StoreError> {
        let unique: std::collections::HashSet<MediaId> = ids.iter().copied().collect();
        let ids: Vec<MediaId> = unique.into_iter().collect();
        let mut out = Vec::new();
        for chunk in ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = coverage_sql(&format!("WHERE f.media_id IN ({placeholders})"));
            out.extend(self.read_coverage(
                &sql,
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
            )?);
        }
        Ok(out)
    }

    pub fn file_coverage_by_id(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<FileCoverage>, StoreError> {
        let sql = coverage_sql("WHERE f.file_id = ?1");
        Ok(self
            .read_coverage(&sql, [file_id.to_string()])?
            .into_iter()
            .next())
    }

    pub fn file_coverage_in_library(
        &self,
        library: LibraryId,
        media: Option<MediaId>,
        scope: FileReadScope,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        let predicate = match scope {
            FileReadScope::Playable => {
                " AND f.publication_state = 'published' AND f.presence_state = 'present'"
            }
            FileReadScope::Published => " AND f.publication_state = 'published'",
            FileReadScope::AllRecorded => "",
        };
        let media_filter = if media.is_some() {
            " AND f.media_id = ?2"
        } else {
            ""
        };
        let sql = coverage_sql(&format!("WHERE f.library_id = ?1{media_filter}{predicate}"));
        match media {
            Some(media) => {
                self.read_coverage(&sql, params![library.to_string(), media.to_string()])
            }
            None => self.read_coverage(&sql, params![library.to_string()]),
        }
    }

    pub fn media_ids_in_libraries(
        &self,
        libraries: &[LibraryId],
    ) -> Result<Vec<MediaId>, StoreError> {
        let mut out = Vec::new();
        for chunk in libraries.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; chunk.len()].join(",");
            let mut stmt = self.library.prepare(&format!(
                "SELECT DISTINCT media_id FROM library_items WHERE library_id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(LibraryId::to_string)),
                |row| row.get::<_, String>(0),
            )?;
            for row in rows {
                out.push(row?.parse::<MediaId>().map_err(|_| {
                    StoreError::Invalid("library_items.media_id is not a Media id".into())
                })?);
            }
        }
        out.sort_by_key(MediaId::to_string);
        out.dedup();
        Ok(out)
    }

    pub fn file_media_under(
        &self,
        dir: &std::path::Path,
    ) -> Result<Vec<(MediaId, std::path::PathBuf)>, StoreError> {
        // Same canonical form (root aliases included) the rows were stored with.
        let key: String = self.library.query_row(
            "SELECT norm_path_key(?1)",
            [stored_path_key(&dir.to_string_lossy())],
            |row| row.get(0),
        )?;
        let (low, high) = if key == "/" {
            ("/".to_string(), "0".to_string())
        } else {
            (format!("{key}/"), format!("{key}0"))
        };
        let mut stmt = self.library.prepare(
            "SELECT media_id, display_path FROM library_files
             WHERE path_key >= ?1 AND path_key < ?2",
        )?;
        let rows = stmt.query_map(params![low, high], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (media_id, path) = row?;
            out.push((media_id.parse::<MediaId>()?, std::path::PathBuf::from(path)));
        }
        Ok(out)
    }

    pub fn list_file_coverage(&self) -> Result<Vec<FileCoverage>, StoreError> {
        let sql = coverage_sql("");
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map([], map_file_unit)?;
        let collected = group_file_coverage(rows.collect::<Result<Vec<_>, _>>()?);
        self.root_ledger_scans.fetch_add(
            (stmt.get_status(rusqlite::StatementStatus::FullscanStep) as usize).max(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(collected)
    }

    pub(crate) fn file_ids_of_media(
        &self,
        media_id: MediaId,
    ) -> Result<Vec<(LibraryId, Vec<LedgerId>)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT file_id, library_id FROM library_files WHERE media_id = ?1 ORDER BY file_id",
        )?;
        let rows = stmt.query_map(params![media_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut grouped: Vec<(LibraryId, Vec<LedgerId>)> = Vec::new();
        for row in rows {
            let (file_id, library_id) = row?;
            let library_id = library_id.parse::<LibraryId>()?;
            let file_id = file_id.parse::<LedgerId>()?;
            match grouped.iter_mut().find(|(id, _)| *id == library_id) {
                Some((_, ids)) => ids.push(file_id),
                None => grouped.push((library_id, vec![file_id])),
            }
        }
        Ok(grouped)
    }

    pub fn transfer_pending(&self, path: &str) -> Result<bool, StoreError> {
        let pending: Option<bool> = self
            .library
            .query_row(
                "SELECT publication_state = 'pending' FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [stored_path_key(path)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(pending.unwrap_or(false))
    }

    pub fn ledger_source_path_matches(
        &self,
        path: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        let actual: Option<Option<String>> = self
            .library
            .query_row(
                "SELECT source_path FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [stored_path_key(path)],
                |row| row.get(0),
            )
            .optional()?;
        let actual = actual.flatten();
        Ok(match (actual.as_deref(), expected) {
            (None, None) => true,
            (Some(actual), Some(expected)) => {
                self.recorded_path_key(actual) == self.recorded_path_key(expected)
            }
            _ => false,
        })
    }
}
