//! File-fact reads and writes for the `library_files` model.
//!
//! Reads return the explicit [`FileCoverage`] view: one `LibraryFile` plus every unit
//! coordinate recorded for it. Nothing here takes a compact (dashless) id string — the
//! API boundary converts request ids to `LedgerId` once and calls the typed accessors.

use domain::{FileCoverage, FileVersion, LedgerId, LibraryId, MediaId, UnitKey};
use rusqlite::{OptionalExtension, params};

use super::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit};
use super::{Store, StoreError};

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
    /// One file's coverage, grouped from the file-fact projection.
    fn read_coverage<P: rusqlite::Params>(
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

    /// 按路径取文件覆盖（扫描入库时判断「已记录」用）。
    pub fn file_coverage_by_path(&self, path: &str) -> Result<Option<FileCoverage>, StoreError> {
        let sql = coverage_sql(
            "WHERE f.file_id = (SELECT file_id FROM library_files
                                WHERE path_key = norm_path_key(?1) LIMIT 1)",
        );
        Ok(self.read_coverage(&sql, [path])?.into_iter().next())
    }

    /// The file recorded at `path` and its version, for version-checked artwork publication.
    pub fn file_version_for_path(
        &self,
        path: &str,
    ) -> Result<Option<(LedgerId, FileVersion)>, StoreError> {
        self.library
            .query_row(
                "SELECT file_id, file_revision, source_version FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [path],
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

    /// 一次取多个 Media 的文件覆盖（≤500 个 id 一块，复用 `library_files.media_id` 索引）。
    /// 重复 id 只查询一次，输出不保证顺序。
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

    /// One file's coverage by its typed id; an unknown id reads as `None`.
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

    /// Every file with its unit coverage, plus the unfiltered-scan work the read cost.
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

    /// Remove every file row of `media_id`, whichever Library owns it.
    pub fn delete_ledger_for_media(&self, media_id: MediaId) -> Result<usize, StoreError> {
        let owned = self.file_ids_of_media(media_id)?;
        let mut removed = 0;
        for (library_id, file_ids) in owned {
            removed += self.delete_files(library_id, &file_ids)?;
        }
        Ok(removed)
    }

    /// Remove the file row recorded at `path` (no-op when nothing is recorded there).
    pub fn delete_ledger_path(&self, path: &str) -> Result<(), StoreError> {
        let target: Option<(String, String)> = self
            .library
            .query_row(
                "SELECT file_id, library_id FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((file_id, library_id)) = target {
            self.delete_files(
                library_id.parse::<LibraryId>()?,
                &[file_id.parse::<LedgerId>()?],
            )?;
        }
        Ok(())
    }

    /// Group this media's file ids by owning Library.
    fn file_ids_of_media(
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
}

impl Store {
    /// Whether the path holds a reserved (not yet published) Transfer row.
    pub fn transfer_pending(&self, path: &str) -> Result<bool, StoreError> {
        let pending: Option<bool> = self
            .library
            .query_row(
                "SELECT publication_state = 'pending' FROM library_files
                 WHERE path_key = norm_path_key(?1)
                 LIMIT 1",
                [path],
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
                [path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(actual.flatten().as_deref() == expected)
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
            .find(|root| root.library_id.as_deref() == Some(library.id.as_str()))
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
