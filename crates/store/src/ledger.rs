//! File-fact reads and writes for the `library_files` model.
//!
//! Reads return the explicit [`FileCoverage`] view: one `LibraryFile` plus every unit
//! coordinate recorded for it. Nothing here takes a compact (dashless) id string — the
//! API boundary converts request ids to `LedgerId` once and calls the typed accessors.

use domain::{FileCoverage, FileVersion, LedgerId, LibraryId, MediaId, UnitKey};
use rusqlite::{OptionalExtension, params};

use super::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit};
use super::{Store, StoreError};

#[path = "ledger/admin_read.rs"]
mod admin_read;
pub use admin_read::LedgerRow;

#[path = "ledger/coverage_page.rs"]
mod coverage_page;

/// Which recorded files a request read sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileReadScope {
    /// Published and present on disk.
    Playable,
    /// Published, including missing/unknown presence.
    Published,
    /// Every recorded file, including pending publication.
    AllRecorded,
}

/// File facts plus the joined unit coordinate; callers group the rows into coverage.
fn coverage_sql(filter: &str) -> String {
    format!(
        "SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode
         FROM library_files f
         LEFT JOIN library_file_units u ON f.file_id = u.file_id
         {filter}"
    )
}

impl Store {
    /// Read related Library facts from a single SQLite snapshot.
    pub fn library_read_snapshot<T>(
        &self,
        read: impl FnOnce(&crate::LibraryConnection<'_>) -> T,
    ) -> Result<T, StoreError> {
        self.with_library_read(|db| db.library_read_snapshot(read))
    }

    /// Whether any recorded file of a Media belongs to this Library.
    pub fn has_file_in_library(
        &self,
        library: LibraryId,
        media: MediaId,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.has_file_in_library(library, media))
    }

    /// Recorded file coverage restricted to one Library, Media and season.
    pub fn file_coverage_in_library_season(
        &self,
        library: LibraryId,
        media: MediaId,
        season: u32,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_in_library_season(library, media, season))
    }

    /// 按路径取文件覆盖（扫描入库时判断「已记录」用）。
    pub fn file_coverage_by_path(&self, path: &str) -> Result<Option<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_by_path(path))
    }

    /// The file recorded at `path` and its version, for version-checked artwork publication.
    pub fn file_version_for_path(
        &self,
        path: &str,
    ) -> Result<Option<(LedgerId, FileVersion)>, StoreError> {
        self.with_library_read(|db| db.file_version_for_path(path))
    }

    pub fn get_file_units(&self, file_id: LedgerId) -> Result<Vec<UnitKey>, StoreError> {
        self.with_library_read(|db| db.get_file_units(file_id))
    }

    pub fn file_coverage_for_media(
        &self,
        media_id: MediaId,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_for_media(media_id))
    }

    /// 一次取多个 Media 的文件覆盖（≤500 个 id 一块，复用 `library_files.media_id` 索引）。
    /// 重复 id 只查询一次，输出不保证顺序。
    pub fn file_coverage_for_media_batch(
        &self,
        ids: &[MediaId],
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_for_media_batch(ids))
    }

    /// One file's coverage by its typed id; an unknown id reads as `None`.
    pub fn file_coverage_by_id(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_by_id(file_id))
    }

    /// Files persisted as owned by `library` (optionally one Media) in `scope`.
    ///
    /// Ownership is the row's `library_id`; the scope predicate runs in SQL.
    pub fn file_coverage_in_library(
        &self,
        library: LibraryId,
        media: Option<MediaId>,
        scope: FileReadScope,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.file_coverage_in_library(library, media, scope))
    }

    /// Distinct Media holding an item in any of `libraries`.
    pub fn media_ids_in_libraries(
        &self,
        libraries: &[LibraryId],
    ) -> Result<Vec<MediaId>, StoreError> {
        self.with_library_read(|db| db.media_ids_in_libraries(libraries))
    }

    /// `(media_id, path)` of every file at or below `dir`, by an index range on
    /// `path_key` — no unit coverage and no whole-Library scan (L1a).
    pub fn file_media_under(
        &self,
        dir: &std::path::Path,
    ) -> Result<Vec<(MediaId, std::path::PathBuf)>, StoreError> {
        self.with_library_read(|db| db.file_media_under(dir))
    }

    /// Read at most 200 complete file coverage records after a Library-local cursor.
    pub fn list_file_coverage_page(
        &self,
        library: LibraryId,
        after: Option<LedgerId>,
        limit: usize,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|connection| coverage_page::read(connection, library, after, limit))
    }

    /// Every file with its unit coverage, plus the unfiltered-scan work the read cost.
    pub fn list_file_coverage(&self) -> Result<Vec<FileCoverage>, StoreError> {
        self.with_library_read(|db| db.list_file_coverage())
    }

    /// Remove every file row of `media_id`, whichever Library owns it.
    /// Remove the file row recorded at `path` (no-op when nothing is recorded there).
    pub fn delete_ledger_path(&self, path: &str) -> Result<(), StoreError> {
        let target: Option<(String, String)> = self.with_read(crate::Db::Library, |library| {
            Ok(library
                .query_row(
                    "SELECT file_id, library_id FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                    [stored_path_key(path)],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })?;
        if let Some((file_id, library_id)) = target {
            self.delete_files(
                library_id.parse::<LibraryId>()?,
                &[file_id.parse::<LedgerId>()?],
            )?;
        }
        Ok(())
    }
}

impl Store {
    /// Whether the path holds a reserved (not yet published) Transfer row.
    pub fn transfer_pending(&self, path: &str) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.transfer_pending(path))
    }

    pub fn ledger_source_path_matches(
        &self,
        path: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.ledger_source_path_matches(path, expected))
    }

    /// Rewrite every fact path equal to `from` into `to` for a media's
    /// subscribes（整理后 wash-cut 的事实路径仍指向新文件）。
    pub fn rewrite_fact_paths(
        &self,
        media_id: domain::MediaId,
        from: &str,
        to: &str,
    ) -> Result<(), StoreError> {
        for subscribe in self.list_all_subscribes()? {
            if subscribe.media_id != media_id {
                continue;
            }
            let mut facts = self.load_subscribe_facts(subscribe.id)?;
            let mut changed = false;
            for entry in facts.entries_mut() {
                let fact = entry.1;
                if fact.path.as_deref() == Some(from) {
                    fact.path = Some(to.to_string());
                    changed = true;
                }
            }
            if changed {
                self.save_subscribe_facts(subscribe.id, &facts)?;
            }
        }
        Ok(())
    }
}

fn stored_path_key(path: &str) -> String {
    crate::path_key::normalize_path_key(path)
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::library_write::{FilePublication as PublicationRequest, WriteScope};
    use domain::{
        Confidence, FilePresence, FilePublication, LibraryFile, MediaKind, QualitySource,
    };

    fn seed_media(store: &Store, index: usize) -> MediaId {
        let media = domain::Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: format!("Batch {index}"),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: Vec::new(),
        };
        store.insert_media(&media).unwrap();
        media.id
    }

    fn seed_file(store: &Store, library_id: &str, root_id: &str, media_id: MediaId, path: &str) {
        let file = LibraryFile {
            file_id: LedgerId::new(),
            library_id: library_id.parse().unwrap(),
            root_id: root_id.to_string(),
            media_id,
            kind: MediaKind::Movie,
            path: path.into(),
            version: FileVersion {
                revision: 1,
                source_version: format!("v-{path}"),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        };
        let scope = WriteScope {
            library_id: library_id.parse().unwrap(),
            config_revision: 0,
        };
        store
            .publish_files(
                &scope,
                &[PublicationRequest {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();
    }

    #[test]
    fn file_coverage_for_media_batch_covers_chunk_boundary_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let store = Store::open(&data).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[tmp.path().join("movies").to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.library_id.as_str() == library.id.as_str())
            .unwrap();

        let ids: Vec<MediaId> = (0..501).map(|index| seed_media(&store, index)).collect();
        for (index, media_id) in ids.iter().enumerate() {
            let path = format!("{}/Batch.{index:04}.mkv", root.path.display());
            seed_file(&store, &library.id, &root.id, *media_id, &path);
        }

        let coverage = store.file_coverage_for_media_batch(&ids).unwrap();
        assert_eq!(coverage.len(), 501);
        assert!(
            coverage
                .iter()
                .all(|file| file.units == vec![UnitKey::WHOLE])
        );
        assert!(store.file_coverage_for_media_batch(&[]).unwrap().is_empty());

        // 重复 id 只查询一次，不产生重复覆盖。
        let mut duplicated = ids.clone();
        duplicated.extend(ids.iter().take(10).copied());
        let deduped = store.file_coverage_for_media_batch(&duplicated).unwrap();
        assert_eq!(deduped.len(), 501);
    }
}

#[path = "ledger/connection.rs"]
mod connection;
