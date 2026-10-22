use std::path::{Path, PathBuf};

use domain::{Confidence, MediaKind};
use rusqlite::{OptionalExtension, params};

pub mod markers;
pub use markers::{MarkerResultReplacement, StoredMediaMarker};

pub(super) const CURRENT_TRACKS_VERSION: i64 = 1;
const SETTING_TRANSFER_MODE: &str = "transfer_mode";

use super::{Store, StoreError};

impl Store {
    pub fn naming_template(&self, kind: MediaKind) -> Result<String, StoreError> {
        self.app
            .query_row(
                "SELECT pattern FROM naming_templates WHERE kind = ?1",
                params![kind.as_str()],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Primary root of the kind's default library — where new content lands.
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

    /// Replace the primary root of the kind's default library.
    pub fn set_library_root(&self, kind: MediaKind, path: &str) -> Result<(), StoreError> {
        self.set_library_roots(&[(kind, path)])
    }

    /// Replace the primary roots of several default libraries in one transaction.
    ///
    /// A directory edit writes Movie and TV together; a failure on the second
    /// root must not leave the first one replaced.
    pub fn set_library_roots(&self, roots: &[(MediaKind, &str)]) -> Result<(), StoreError> {
        if roots.is_empty() {
            return Ok(());
        }
        let paths: Vec<&str> = roots.iter().map(|(_, path)| *path).collect();
        for (kind, _) in roots {
            if let Some(library) = self.default_library(*kind)? {
                self.ensure_library_mutable(&library.id)?;
            }
        }
        self.ensure_removal_roots_available(&paths)?;
        let tx = self.library.unchecked_transaction()?;
        for (kind, path) in roots {
            let root_id: String = tx.query_row(
                "SELECT r.root_id FROM library_roots r
                 JOIN libraries l ON l.library_id = r.library_id
                 WHERE l.kind = ?1 AND l.is_default = 1
                 ORDER BY r.sort_order, r.display_path LIMIT 1",
                params![kind.as_str()],
                |row| row.get(0),
            )?;
            let changed = tx.execute(
                "UPDATE library_roots SET path_key = ?3, display_path = ?2 WHERE root_id = ?1",
                params![root_id, path, crate::path_key::canonical_root_key(path)?],
            )?;
            crate::path_key::reload_root_aliases(&tx)?;
            if changed == 0 {
                return Err(StoreError::Missing(format!("library root {root_id}")));
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_naming_template(&self, kind: MediaKind, pattern: &str) -> Result<(), StoreError> {
        self.app.execute(
            "UPDATE naming_templates SET pattern = ?2 WHERE kind = ?1",
            params![kind.as_str(), pattern],
        )?;
        Ok(())
    }

    pub fn transfer_mode(&self) -> Result<String, StoreError> {
        Ok(self
            .get_setting(SETTING_TRANSFER_MODE)?
            .unwrap_or_else(|| "hardlink".into()))
    }

    pub fn set_transfer_mode(&self, mode: &str) -> Result<(), StoreError> {
        self.put_setting(SETTING_TRANSFER_MODE, mode)
    }

    pub fn scrape_enabled(&self) -> Result<bool, StoreError> {
        Ok(self
            .get_setting("scrape")?
            .map(|value| value == "1" || value == "true")
            .unwrap_or(false))
    }

    pub fn set_scrape_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.put_setting("scrape", if enabled { "1" } else { "0" })
    }

    pub fn watch_intake(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .get_setting("watch_intake")?
            .filter(|path| !path.is_empty()))
    }

    pub fn set_watch_intake(&self, path: &str) -> Result<(), StoreError> {
        self.put_setting("watch_intake", path.trim())
    }

    pub fn watch_inplace(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .get_setting("watch_inplace")?
            .filter(|path| !path.is_empty()))
    }

    pub fn set_watch_inplace(&self, path: &str) -> Result<(), StoreError> {
        self.put_setting("watch_inplace", path.trim())
    }

    /// 读取文件的流信息缓存（探测任务写入）。
    ///
    /// 缓存按源版本有效：文件内容被重扫替换后，旧流的行内事实不再描述磁盘上的文件，
    /// 因此这里返回 None 让调用方重新探测，而不是把上一代的流信息当成现状。
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

    /// 写入流信息缓存。
    ///
    /// 行携带文件当前的 (source_revision, source_version)：缓存永远标注它描述的是哪一代文件。
    /// `source_version` 非空时是调用方探测所用的版本，与文件当前版本不符即视为作废结果，忽略写入。
    pub fn put_file_meta_versioned(
        &self,
        ledger_id: &str,
        tracks: &library::Tracks,
        source_version: Option<&str>,
        format_duration_ms: Option<i64>,
    ) -> Result<(), StoreError> {
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

    /// 写入章节缓存（标记检测与探测共用同一写入口）。
    pub fn put_cached_chapters(
        &self,
        ledger_id: &str,
        chapters: &[library::ChapterMarker],
    ) -> Result<(), StoreError> {
        let json_str = serde_json::to_string(chapters)?;
        if !upsert_chapter_cache_tx(&self.library, ledger_id, &json_str)? {
            tracing::warn!(ledger_id, "章节缓存写入被忽略：文件行不存在或源版本已变化");
        }
        Ok(())
    }

    pub fn clear_cached_chapters(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE file_details SET chapters_json = NULL WHERE file_id = ?1",
            params![ledger_id],
        )?;
        Ok(())
    }

    pub fn delete_file_meta_by_ledger_id(&self, ledger_id: &str) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        delete_file_meta_for_ledger(&tx, ledger_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_unidentified(
        &self,
        path: String,
        confidence: Confidence,
    ) -> Result<(), StoreError> {
        // v1 scopes the parked row to a Library root. Files parked by intake may
        // still sit outside every root, so fall back to the default Movie root:
        // that is the Library they would be claimed into.
        let (library_id, root_id) = self.unidentified_scope(&path)?;
        self.library.execute(
            "INSERT INTO unidentified (library_id, root_id, path, confidence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(library_id, path) DO UPDATE SET
                 root_id = excluded.root_id,
                 confidence = excluded.confidence",
            params![library_id, root_id, path, confidence.as_str()],
        )?;
        Ok(())
    }

    fn unidentified_scope(&self, path: &str) -> Result<(String, String), StoreError> {
        let resolved = crate::library_paths::resolved_path(Path::new(path))?;
        let roots = self.list_library_roots()?;
        // A root that cannot be resolved is an error, not a silent miss: falling
        // back to the default root would park the file under the wrong Library.
        let mut best: Option<(usize, String, String)> = None;
        for root in &roots {
            let root_path = crate::library_paths::resolved_path(&root.path)?;
            let Some(library_id) = root.library_id.clone() else {
                continue;
            };
            if !resolved.starts_with(&root_path) {
                continue;
            }
            let depth = root_path.components().count();
            if best
                .as_ref()
                .is_none_or(|(best_depth, _, _)| depth > *best_depth)
            {
                best = Some((depth, library_id, root.id.clone()));
            }
        }
        if let Some((_, library_id, root_id)) = best {
            return Ok((library_id, root_id));
        }
        let default_root = roots
            .iter()
            .find(|root| root.is_default && root.kind == domain::MediaKind::Movie)
            .and_then(|root| root.library_id.clone().map(|id| (id, root.id.clone())))
            .ok_or_else(|| {
                StoreError::Missing(format!(
                    "no Movie Library root to park unidentified path {path}"
                ))
            })?;
        Ok(default_root)
    }

    pub fn list_unidentified(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path, confidence FROM unidentified ORDER BY path, library_id")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_unidentified(&self, path: &str) -> Result<bool, StoreError> {
        let n = self
            .library
            .execute("DELETE FROM unidentified WHERE path = ?1", params![path])?;
        Ok(n > 0)
    }
}

impl Store {
    /// 全库文件核验：路径不存在的文件行标 missing_at=now，恢复的文件清掉标记。
    pub fn verify_library_files(
        &self,
        roots: &[std::path::PathBuf],
        now: i64,
    ) -> Result<(i64, i64), StoreError> {
        let mut missing = 0;
        let mut restored = 0;
        for (file_id, path) in self.file_paths()? {
            if !roots
                .iter()
                .any(|root| std::path::Path::new(&path).starts_with(root))
            {
                continue;
            }
            if std::path::Path::new(&path).exists() {
                let n = self.library.execute(
                    "UPDATE library_files SET missing_at = NULL WHERE file_id = ?1 AND missing_at IS NOT NULL",
                    params![file_id],
                )?;
                if n > 0 {
                    restored += 1;
                }
            } else {
                let n = self.library.execute(
                    "UPDATE library_files SET missing_at = ?1 WHERE file_id = ?2 AND missing_at IS NULL",
                    params![now, file_id],
                )?;
                if n > 0 {
                    missing += 1;
                }
            }
        }
        Ok((missing, restored))
    }

    /// path → missing_at 映射（接口打「文件缺失」标用）。
    pub fn missing_at_by_path(&self) -> Result<std::collections::HashMap<String, i64>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT display_path, missing_at FROM library_files WHERE missing_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 删除缺失文件的事实行（仅限这些根目录内），并在同一事务里重算受影响的摘要。
    pub fn delete_missing_rows(&self, roots: &[std::path::PathBuf]) -> Result<usize, StoreError> {
        let targets = self
            .missing_files()?
            .into_iter()
            .filter(|file| {
                roots
                    .iter()
                    .any(|root| std::path::Path::new(&file.path).starts_with(root))
            })
            .collect::<Vec<_>>();
        self.delete_file_facts(&targets)
    }

    /// 删除指定媒体的缺失文件事实行（仅限这些根目录内）。
    pub fn delete_missing_rows_for_media(
        &self,
        roots: &[std::path::PathBuf],
        media_id: domain::MediaId,
    ) -> Result<usize, StoreError> {
        let targets = self
            .missing_files()?
            .into_iter()
            .filter(|file| {
                file.media_id == media_id
                    && roots
                        .iter()
                        .any(|root| std::path::Path::new(&file.path).starts_with(root))
            })
            .collect::<Vec<_>>();
        self.delete_file_facts(&targets)
    }

    /// 该 Media 是否有「文件缺失」的文件行（transfer 据此决定是否重链）。
    pub fn media_has_missing(&self, media_id: domain::MediaId) -> Result<bool, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT 1 FROM library_files WHERE media_id = ?1 AND missing_at IS NOT NULL LIMIT 1",
                params![media_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// 文件重新入库后清除缺失标记（摘要随事实在同一事务里重算）。
    pub fn clear_ledger_missing(&self, path: &str) -> Result<bool, StoreError> {
        let tx = self.library.unchecked_transaction()?;
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

    /// Every file row as `(file_id, display_path)`.
    fn file_paths(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT file_id, display_path FROM library_files")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// File rows marked missing, with the owner needed to reduce their summaries.
    fn missing_files(&self) -> Result<Vec<MissingFile>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT file_id, library_id, media_id, display_path
             FROM library_files WHERE missing_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut files = Vec::new();
        for row in rows {
            let (file_id, library_id, media_id, path) = row?;
            files.push(MissingFile {
                file_id,
                library_id: library_id.parse()?,
                media_id: media_id.parse()?,
                path,
            });
        }
        Ok(files)
    }

    /// Remove file facts and reduce the summaries they fed, in one transaction.
    fn delete_file_facts(&self, targets: &[MissingFile]) -> Result<usize, StoreError> {
        if targets.is_empty() {
            return Ok(0);
        }
        let tx = self.library.unchecked_transaction()?;
        for file in targets {
            tx.execute(
                "DELETE FROM library_files WHERE file_id = ?1",
                params![file.file_id],
            )?;
        }
        let mut owners = std::collections::HashSet::new();
        for file in targets {
            owners.insert((file.library_id, file.media_id));
        }
        for (library_id, media_id) in owners {
            crate::library_write::reduce::reduce_library_media(&tx, library_id, media_id)?;
        }
        tx.commit()?;
        Ok(targets.len())
    }
}

/// A file row that is on disk no longer, with the owner needed for summary reduction.
struct MissingFile {
    file_id: String,
    library_id: domain::LibraryId,
    media_id: domain::MediaId,
    path: String,
}

/// Remove all caches in the same transaction as ledger deletion when applicable.
pub(crate) fn delete_file_meta_for_ledger(
    tx: &rusqlite::Transaction<'_>,
    ledger_id: &str,
) -> Result<(), StoreError> {
    tx.execute("DELETE FROM file_details WHERE file_id = ?1", [ledger_id])?;
    delete_derived_contexts_for_ledger(tx, ledger_id)
}

/// Drop context-keyed derived state (fingerprints, attempts, probe stages,
/// samples) of one file; cold details are left to the caller.
pub(crate) fn delete_derived_contexts_for_ledger(
    tx: &rusqlite::Transaction<'_>,
    ledger_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM fingerprint_cache WHERE file_id = ?1",
        [ledger_id],
    )?;
    tx.execute(
        "DELETE FROM fingerprint_attempts WHERE file_id = ?1",
        [ledger_id],
    )?;
    super::probe_state::delete_probe_stages_for_ledger(tx, ledger_id)?;
    super::fingerprint_samples::delete_samples_for_ledger(tx, ledger_id)
}

/// 章节缓存的唯一写入口。
///
/// 行不存在时按文件当前 (revision, source_version) 建行；已存在且版本不同，说明这行（含其
/// 章节）描述的是旧内容，本次结果不再匹配当前文件，忽略而不覆盖。返回是否写入。
pub(crate) fn upsert_chapter_cache_tx(
    tx: &rusqlite::Connection,
    file_id: &str,
    chapters_json: &str,
) -> Result<bool, StoreError> {
    let changed = tx.execute(
        "INSERT INTO file_details (
             file_id, source_revision, source_version, tracks_version,
             audio_json, subtitle_json, chapters_json
         )
         SELECT f.file_id, f.file_revision, f.source_version, 0, '[]', '[]', ?2
         FROM library_files f
         WHERE f.file_id = ?1
         ON CONFLICT(file_id) DO UPDATE SET chapters_json = excluded.chapters_json
         WHERE file_details.source_version = excluded.source_version",
        params![file_id, chapters_json],
    )?;
    Ok(changed > 0)
}
