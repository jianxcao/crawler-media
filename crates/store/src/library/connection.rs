//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn library_root(&self, kind: MediaKind) -> Result<PathBuf, StoreError> {
        let path: String = self.library.query_row(
            "SELECT r.display_path FROM library_roots r
             JOIN libraries l ON l.library_id = r.library_id
             WHERE l.kind = ?1 AND l.is_default = 1
             ORDER BY r.sort_order, r.display_path LIMIT 1",
            params![kind.as_str()],
            |row| row.get(0),
        )?;
        Ok(PathBuf::from(path))
    }

    pub fn get_file_meta(&self, ledger_id: &str) -> Result<Option<library::Tracks>, StoreError> {
        self.library
            .query_row(
                "SELECT d.audio_json, d.subtitle_json, d.video_json, d.tracks_version
                 FROM file_details d
                 JOIN library_files f ON f.file_id = d.file_id
                 WHERE d.file_id = ?1 AND d.source_version = f.source_version",
                params![ledger_id],
                |row| {
                    let video_raw: Option<String> = row.get(2).ok().flatten();
                    let tracks_version: i64 = row.get(3)?;
                    if tracks_version < CURRENT_TRACKS_VERSION {
                        return Ok(None);
                    }
                    // 旧版缓存行没有 video_json（NULL）：视为「未完成探测」，
                    // 返回 None 让调用方重新探测并补写完整流信息。
                    if video_raw.is_none() {
                        return Ok(None);
                    }
                    let audio =
                        serde_json::from_str::<Vec<library::AudioTrack>>(&row.get::<_, String>(0)?)
                            .unwrap_or_default();
                    let subtitles = serde_json::from_str::<Vec<library::SubtitleTrack>>(
                        &row.get::<_, String>(1)?,
                    )
                    .unwrap_or_default();
                    let video = video_raw
                        .and_then(|s| serde_json::from_str::<library::VideoTrack>(&s).ok());
                    Ok(Some(library::Tracks {
                        video,
                        audio,
                        subtitles,
                    }))
                },
            )
            .optional()
            .map(|opt| opt.flatten())
            .map_err(Into::into)
    }

    pub fn put_file_meta(
        &self,
        ledger_id: &str,
        tracks: &library::Tracks,
    ) -> Result<(), StoreError> {
        self.put_file_meta_versioned(ledger_id, tracks, None, None)
    }

    pub fn put_file_meta_versioned(
        &self,
        ledger_id: &str,
        tracks: &library::Tracks,
        source_version: Option<&str>,
        format_duration_ms: Option<i64>,
    ) -> Result<(), StoreError> {
        let tx = self
            .library
            .is_autocommit()
            .then(|| crate::write_tx(self.library))
            .transpose()?;
        ensure_file_integrity_writable(self.library, ledger_id)?;
        let changed = self.library.execute(
            "INSERT INTO file_details (
                 file_id, source_revision, source_version, tracks_version,
                 audio_json, subtitle_json, video_json, format_duration_ms
             )
             SELECT f.file_id, f.file_revision, f.source_version, ?5, ?2, ?3, ?4, ?7
             FROM library_files f
             WHERE f.file_id = ?1 AND (?6 IS NULL OR f.source_version = ?6)
             ON CONFLICT(file_id) DO UPDATE SET
               source_revision = excluded.source_revision,
               source_version = excluded.source_version,
               audio_json = excluded.audio_json,
               subtitle_json = excluded.subtitle_json,
               video_json = excluded.video_json,
               tracks_version = excluded.tracks_version,
               format_duration_ms = excluded.format_duration_ms",
            params![
                ledger_id,
                serde_json::to_string(&tracks.audio)?,
                serde_json::to_string(&tracks.subtitles)?,
                tracks
                    .video
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
                CURRENT_TRACKS_VERSION,
                source_version,
                format_duration_ms,
            ],
        )?;
        if changed == 0 {
            tracing::warn!(
                ledger_id,
                source_version,
                "流信息缓存写入被忽略：文件行不存在或源版本已变化"
            );
        }
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn get_cached_chapters(
        &self,
        ledger_id: &str,
    ) -> Result<Option<Vec<library::ChapterMarker>>, StoreError> {
        self.library
            .query_row(
                "SELECT d.chapters_json
                 FROM file_details d
                 JOIN library_files f ON f.file_id = d.file_id
                 WHERE d.file_id = ?1 AND d.source_version = f.source_version",
                params![ledger_id],
                |row| {
                    let json_str: Option<String> = row.get(0)?;
                    Ok(json_str.and_then(|s| serde_json::from_str(&s).ok()))
                },
            )
            .optional()
            .map(|opt| opt.flatten())
            .map_err(Into::into)
    }

    pub fn put_cached_chapters(
        &self,
        ledger_id: &str,
        chapters: &[library::ChapterMarker],
    ) -> Result<(), StoreError> {
        let tx = self
            .library
            .is_autocommit()
            .then(|| crate::write_tx(self.library))
            .transpose()?;
        ensure_file_integrity_writable(self.library, ledger_id)?;
        let json_str = serde_json::to_string(chapters)?;
        if !upsert_chapter_cache_tx(self.library, ledger_id, &json_str)? {
            tracing::warn!(ledger_id, "章节缓存写入被忽略：文件行不存在或源版本已变化");
        }
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn clear_cached_chapters(&self, ledger_id: &str) -> Result<(), StoreError> {
        let tx = self
            .library
            .is_autocommit()
            .then(|| crate::write_tx(self.library))
            .transpose()?;
        ensure_file_integrity_writable(self.library, ledger_id)?;
        self.library.execute(
            "UPDATE file_details SET chapters_json = NULL WHERE file_id = ?1",
            params![ledger_id],
        )?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn delete_file_meta_by_ledger_id(&self, ledger_id: &str) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        ensure_file_integrity_writable(&tx, ledger_id)?;
        delete_file_meta_for_ledger(&tx, ledger_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Apply only observations whose identity and prior missing marker survived IO.
    pub(super) fn apply_file_verification(
        &self,
        observed: &[(VerificationFile, bool)],
        now: i64,
    ) -> Result<(i64, i64), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let mut current = Vec::new();
        for (file, exists) in observed {
            let matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM library_files
                 WHERE file_id=?1 AND display_path=?2 AND library_id=?3 AND root_id=?4
                   AND media_id=?5 AND file_revision=?6 AND source_version=?7
                   AND missing_at IS ?8 AND publication_state=?9)",
                params![
                    file.file_id,
                    file.path,
                    file.library_id,
                    file.root_id,
                    file.media_id,
                    file.revision,
                    file.source_version,
                    file.missing_at,
                    file.publication
                ],
                |row| row.get(0),
            )?;
            if matches {
                ensure_file_integrity_writable(&tx, &file.file_id)?;
                current.push((file, exists));
            }
        }
        let mut missing = 0;
        let mut restored = 0;
        for (file, exists) in current {
            if *exists {
                restored += tx.execute(
                    "UPDATE library_files SET missing_at = NULL
                     WHERE file_id = ?1 AND missing_at IS NOT NULL",
                    params![file.file_id],
                )? as i64;
            } else {
                missing += tx.execute(
                    "UPDATE library_files SET missing_at = ?1
                     WHERE file_id = ?2 AND missing_at IS NULL",
                    params![now, file.file_id],
                )? as i64;
            }
        }
        tx.commit()?;
        Ok((missing, restored))
    }

    pub fn clear_ledger_missing(&self, path: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let mut owners = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT library_id, media_id FROM library_files
                 WHERE display_path = ?1 AND (missing_at IS NOT NULL OR presence_state <> 'present')",
            )?;
            let rows = stmt.query_map(params![path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (library_id, media_id) = row?;
                owners.push((
                    library_id.parse::<domain::LibraryId>()?,
                    media_id.parse::<domain::MediaId>()?,
                ));
            }
        }
        for (_, media) in &owners {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        let changed = tx.execute(
            "UPDATE library_files SET missing_at = NULL, presence_state = 'present'
             WHERE display_path = ?1 AND (missing_at IS NOT NULL OR presence_state <> 'present')",
            params![path],
        )?;
        for (library_id, media_id) in owners {
            crate::library_write::reduce::reduce_library_media(&tx, library_id, media_id)?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    pub(super) fn file_paths(&self) -> Result<Vec<VerificationFile>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT file_id, display_path, library_id, root_id, media_id,
                    file_revision, source_version, missing_at, publication_state
             FROM library_files",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(VerificationFile {
                file_id: row.get(0)?,
                path: row.get(1)?,
                library_id: row.get(2)?,
                root_id: row.get(3)?,
                media_id: row.get(4)?,
                revision: row.get(5)?,
                source_version: row.get(6)?,
                missing_at: row.get(7)?,
                publication: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
