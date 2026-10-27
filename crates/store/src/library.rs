use std::path::PathBuf;

use domain::MediaKind;
use rusqlite::{OptionalExtension, params};

pub mod markers;
pub use markers::{
    MarkerResultReplacement, SourceObservation, SourceSnapshot, StoredMediaMarker, observe_sources,
    observe_sources_with,
};

pub(super) const CURRENT_TRACKS_VERSION: i64 = 1;

use super::{Store, StoreError};

impl Store {
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
        let tx = self
            .library
            .is_autocommit()
            .then(|| crate::write_tx(&self.library))
            .transpose()?;
        ensure_file_integrity_writable(&self.library, ledger_id)?;
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

    /// 写入章节缓存（标记检测与探测共用同一写入口）。
    pub fn put_cached_chapters(
        &self,
        ledger_id: &str,
        chapters: &[library::ChapterMarker],
    ) -> Result<(), StoreError> {
        let tx = self
            .library
            .is_autocommit()
            .then(|| crate::write_tx(&self.library))
            .transpose()?;
        ensure_file_integrity_writable(&self.library, ledger_id)?;
        let json_str = serde_json::to_string(chapters)?;
        if !upsert_chapter_cache_tx(&self.library, ledger_id, &json_str)? {
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
            .then(|| crate::write_tx(&self.library))
            .transpose()?;
        ensure_file_integrity_writable(&self.library, ledger_id)?;
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
        let tx = crate::write_tx(&self.library)?;
        ensure_file_integrity_writable(&tx, ledger_id)?;
        delete_file_meta_for_ledger(&tx, ledger_id)?;
        tx.commit()?;
        Ok(())
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
        let observed = self
            .file_paths()?
            .into_iter()
            .filter(|(_, path)| {
                roots
                    .iter()
                    .any(|root| std::path::Path::new(path).starts_with(root))
            })
            .map(|(id, path)| (id, path.clone(), std::path::Path::new(&path).exists()))
            .collect::<Vec<_>>();
        let tx = crate::write_tx(&self.library)?;
        for (id, _, _) in &observed {
            ensure_file_integrity_writable(&tx, id)?;
        }
        for (file_id, path, exists) in observed {
            if !roots
                .iter()
                .any(|root| std::path::Path::new(&path).starts_with(root))
            {
                continue;
            }
            if exists {
                let n = tx.execute(
                    "UPDATE library_files SET missing_at = NULL WHERE file_id = ?1 AND missing_at IS NOT NULL",
                    params![file_id],
                )?;
                if n > 0 {
                    restored += 1;
                }
            } else {
                let n = tx.execute(
                    "UPDATE library_files SET missing_at = ?1 WHERE file_id = ?2 AND missing_at IS NULL",
                    params![now, file_id],
                )?;
                if n > 0 {
                    missing += 1;
                }
            }
        }
        tx.commit()?;
        Ok((missing, restored))
    }

    /// path → missing_at 映射（接口打「文件缺失」标用）。
    pub fn missing_at_by_path(&self) -> Result<std::collections::HashMap<String, i64>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path_key, missing_at FROM library_files WHERE missing_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 文件重新入库后清除缺失标记（摘要随事实在同一事务里重算）。
    pub fn clear_ledger_missing(&self, path: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(&self.library)?;
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

fn ensure_file_integrity_writable(tx: &rusqlite::Connection, file: &str) -> Result<(), StoreError> {
    use rusqlite::OptionalExtension;
    let owner: Option<String> = tx
        .query_row(
            "SELECT media_id FROM library_files WHERE file_id=?1",
            [file],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            tx,
            owner.parse()?,
        )?;
    }
    Ok(())
}
