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

/// Owned facts for verification's read/observe/write boundary.
struct VerificationFile {
    file_id: String,
    path: String,
    library_id: String,
    root_id: String,
    media_id: String,
    revision: i64,
    source_version: String,
    missing_at: Option<i64>,
    publication: String,
}

impl Store {
    /// Primary root of the kind's default library — where new content lands.
    pub fn library_root(&self, kind: MediaKind) -> Result<PathBuf, StoreError> {
        self.with_library_read(|db| db.library_root(kind))
    }

    /// 读取文件的流信息缓存（探测任务写入）。
    ///
    /// 缓存按源版本有效：文件内容被重扫替换后，旧流的行内事实不再描述磁盘上的文件，
    /// 因此这里返回 None 让调用方重新探测，而不是把上一代的流信息当成现状。
    pub fn get_file_meta(&self, ledger_id: &str) -> Result<Option<library::Tracks>, StoreError> {
        self.with_library_read(|db| db.get_file_meta(ledger_id))
    }

    pub fn put_file_meta(
        &self,
        ledger_id: &str,
        tracks: &library::Tracks,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.put_file_meta(ledger_id, tracks))
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
        self.with_library_write(|db| {
            db.put_file_meta_versioned(ledger_id, tracks, source_version, format_duration_ms)
        })
    }

    pub fn get_cached_chapters(
        &self,
        ledger_id: &str,
    ) -> Result<Option<Vec<library::ChapterMarker>>, StoreError> {
        self.with_library_read(|db| db.get_cached_chapters(ledger_id))
    }

    /// 写入章节缓存（标记检测与探测共用同一写入口）。
    pub fn put_cached_chapters(
        &self,
        ledger_id: &str,
        chapters: &[library::ChapterMarker],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.put_cached_chapters(ledger_id, chapters))
    }

    pub fn clear_cached_chapters(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.with_library_write(|db| db.clear_cached_chapters(ledger_id))
    }

    pub fn delete_file_meta_by_ledger_id(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.with_library_write(|db| db.delete_file_meta_by_ledger_id(ledger_id))
    }
}

impl Store {
    /// 全库文件核验：路径不存在的文件行标 missing_at=now，恢复的文件清掉标记。
    pub fn verify_library_files(
        &self,
        roots: &[std::path::PathBuf],
        now: i64,
    ) -> Result<(i64, i64), StoreError> {
        self.verify_library_files_with(roots, now, std::path::Path::exists)
    }

    /// Verify recorded paths with an injected filesystem observer.
    pub fn verify_library_files_with(
        &self,
        roots: &[std::path::PathBuf],
        now: i64,
        mut exists: impl FnMut(&std::path::Path) -> bool,
    ) -> Result<(i64, i64), StoreError> {
        let files = self.with_library_read(|db| db.file_paths())?;
        let observed = files
            .into_iter()
            .filter(|file| {
                roots
                    .iter()
                    .any(|root| std::path::Path::new(&file.path).starts_with(root))
            })
            .map(|file| {
                let present = exists(std::path::Path::new(&file.path));
                (file, present)
            })
            .collect::<Vec<_>>();
        self.with_library_write(|db| db.apply_file_verification(&observed, now))
    }

    /// path → missing_at 映射（接口打「文件缺失」标用）。
    pub fn missing_at_by_path(&self) -> Result<std::collections::HashMap<String, i64>, StoreError> {
        self.with_library_read(|db| db.missing_at_by_path())
    }

    /// 文件重新入库后清除缺失标记（摘要随事实在同一事务里重算）。
    pub fn clear_ledger_missing(&self, path: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.clear_ledger_missing(path))
    }
}

/// Remove all caches in the same transaction as ledger deletion when applicable.
pub(crate) fn delete_file_meta_for_ledger(
    tx: &rusqlite::Connection,
    ledger_id: &str,
) -> Result<(), StoreError> {
    tx.execute("DELETE FROM file_details WHERE file_id = ?1", [ledger_id])?;
    delete_derived_contexts_for_ledger(tx, ledger_id)
}

/// Drop context-keyed derived state (fingerprints, attempts, probe stages,
/// samples) of one file; cold details are left to the caller.
pub(crate) fn delete_derived_contexts_for_ledger(
    tx: &rusqlite::Connection,
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

#[path = "library/connection.rs"]
mod connection;
