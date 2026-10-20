//! Ledger reads and file-fact writes for the `library_files` model.
//!
//! `domain::LedgerRow` stays the flat read shape management views consume; the
//! richer file facts (root, publication/presence state, version, source) live in
//! `library_files` and are read by the callers that need them.

use std::path::Path;

use domain::{
    FilePresence, FilePublication, FileVersion, LedgerId, LedgerRow, LibraryFile, LibraryId,
    MediaId, UnitKey,
};
use rusqlite::{OptionalExtension, params};

use super::maps::map_ledger;
use super::{Store, StoreError};
use crate::library_write::{FilePublication as PublicationRequest, WriteScope};

/// Column projection consumed by [`map_ledger`] (indices 0..=10).
const LEDGER_COLUMNS: &str = "f.file_id, f.media_id, f.display_path, u.season, u.episode, \
     f.resolution, f.codec, f.hdr, f.quality_source, f.confidence, f.filter_score";

impl Store {
    /// 按路径取已有台账行（扫描入库时判断「已记录」用）。
    pub fn ledger_by_path(&self, path: &str) -> Result<Option<LedgerRow>, StoreError> {
        let sql = format!(
            "SELECT {LEDGER_COLUMNS}
             FROM library_files f
             LEFT JOIN library_file_units u ON f.file_id = u.file_id
             WHERE f.path_key = ?1 OR f.display_path = ?1
             LIMIT 1"
        );
        let mut stmt = self.library.prepare(&sql)?;
        stmt.query_row(params![path], map_ledger)
            .optional()
            .map_err(Into::into)
    }

    /// The file recorded at `path` and its version, for version-checked artwork publication.
    pub fn file_version_for_path(
        &self,
        path: &str,
    ) -> Result<Option<(LedgerId, FileVersion)>, StoreError> {
        self.library
            .query_row(
                "SELECT file_id, file_revision, source_version FROM library_files
                 WHERE path_key = ?1 OR display_path = ?1
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

    /// Rename a file's recorded path (整理后落名一致；`path_key` 唯一约束保持）。
    pub fn rename_ledger_path(&self, from: &str, to: &str) -> Result<bool, StoreError> {
        let n = self.library.execute(
            "UPDATE library_files SET path_key = ?2, display_path = ?2
             WHERE path_key = ?1 OR display_path = ?1",
            params![from, to],
        )?;
        Ok(n > 0)
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

    pub fn ledger_for_media(&self, media_id: MediaId) -> Result<Vec<LedgerRow>, StoreError> {
        self.media_ledger_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sql = format!(
            "SELECT {LEDGER_COLUMNS}
             FROM library_files f
             LEFT JOIN library_file_units u ON f.file_id = u.file_id
             WHERE f.media_id = ?1"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params![media_id.to_string()], map_ledger)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 一次取多个 Media 的 ledger 行（≤500 个 id 一块，复用 `library_files.media_id` 索引）。
    /// 重复 id 只查询一次，输出行不保证顺序。
    pub fn ledger_for_media_batch(
        &self,
        ids: &[MediaId],
    ) -> Result<Vec<LedgerRow>, StoreError> {
        let unique: std::collections::HashSet<MediaId> = ids.iter().copied().collect();
        let ids: Vec<MediaId> = unique.into_iter().collect();
        let mut out = Vec::new();
        for chunk in ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT {LEDGER_COLUMNS}
                 FROM library_files f
                 LEFT JOIN library_file_units u ON f.file_id = u.file_id
                 WHERE f.media_id IN ({placeholders})"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
                map_ledger,
            )?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        Ok(out)
    }

    pub fn get_ledger_by_unit(
        &self,
        media_id: MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<LedgerRow>, StoreError> {
        let rows = self.ledger_for_media(media_id)?;
        Ok(rows
            .into_iter()
            .find(|r| r.season == season && r.episode == episode))
    }

    pub fn get_ledger(&self, compact_id: &str) -> Result<Option<LedgerRow>, StoreError> {
        let sql = format!(
            "SELECT {LEDGER_COLUMNS}
             FROM library_files f
             LEFT JOIN library_file_units u ON f.file_id = u.file_id
             WHERE f.file_id = ?1 OR REPLACE(f.file_id, '-', '') = ?1
             LIMIT 1"
        );
        let mut stmt = self.library.prepare(&sql)?;
        stmt.query_row(params![compact_id], map_ledger)
            .optional()
            .map_err(Into::into)
    }

    pub fn list_ledger(&self) -> Result<Vec<LedgerRow>, StoreError> {
        Ok(self
            .list_ledger_with_mode()?
            .into_iter()
            .map(|(row, _, _)| row)
            .collect())
    }

    /// Every file row plus its recorded transfer mode and source path.
    pub fn list_ledger_with_mode(
        &self,
    ) -> Result<Vec<(LedgerRow, Option<String>, Option<String>)>, StoreError> {
        let sql = format!(
            "SELECT {LEDGER_COLUMNS}, f.transfer_mode, f.source_path
             FROM library_files f
             LEFT JOIN library_file_units u ON f.file_id = u.file_id"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let ledger = map_ledger(row)?;
            let mode: Option<String> = row.get(11)?;
            let source_path: Option<String> = row.get(12)?;
            Ok((ledger, mode, source_path))
        })?;
        let collected = rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from);
        self.root_ledger_scans.fetch_add(
            (stmt.get_status(rusqlite::StatementStatus::FullscanStep) as usize).max(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        collected
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
                 WHERE path_key = ?1 OR display_path = ?1
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
    /// Record a scanned/claimed file as Library content.
    ///
    /// Legacy adapter over the canonical publication path: it resolves the owning
    /// Library root, requires the Media to be writable, writes the row and its
    /// coverage, and reduces the affected summaries in one transaction.
    pub fn insert_ledger(&self, row: &LedgerRow) -> Result<(), StoreError> {
        self.insert_ledger_with_mode_and_source(row, None, None)
    }

    /// Save the original completed download path alongside an imported row.
    /// The source is retained for an explicit administrator-triggered
    /// retransfer when the Library copy later goes missing.
    pub fn insert_ledger_with_source(&self, row: &LedgerRow, source: &Path) -> Result<(), StoreError> {
        self.insert_ledger_with_mode_and_source(row, None, source.to_str())
    }

    /// Insert a file row with caller-observed transfer mode and optional source
    /// path, avoiding filesystem metadata inspection under the Store lock.
    pub fn insert_ledger_with_mode_and_source(
        &self,
        row: &LedgerRow,
        transfer_mode: Option<&str>,
        source_path: Option<&str>,
    ) -> Result<(), StoreError> {
        let file = self.place_ledger_row(row, transfer_mode, source_path)?;
        let units = ledger_units(row);
        let locale = crate::library_query::library_display_locale(self, file.library_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let config_revision = self.library_config_revision(&file.library_id.to_string()).unwrap_or(0);
        let observation = crate::metadata::NfoObservation::observe(
            &file.path,
            file.kind,
            locale,
            file.version.source_version.clone(),
            config_revision,
        );
        self.publish_files(
            &WriteScope {
                library_id: file.library_id,
                config_revision: 0,
            },
            &[PublicationRequest {
                file: &file,
                units: &units,
                expected_version: None,
                nfo_observation: Some(&observation),
                probe_required: false,
            }],
            crate::unix_now() * 1000,
        )?;
        Ok(())
    }

    /// Finalize a previously reserved Transfer row without recreating it if detached.
    pub fn finalize_transfer(
        &self,
        row: &LedgerRow,
        transfer_mode: &str,
        source_path: Option<&str>,
    ) -> Result<(), StoreError> {
        let file_id = row.id.to_string();
        let reservation: Option<(String, String, Option<String>, i64, String)> = self
            .library
            .query_row(
                "SELECT media_id, path_key, source_path, file_revision, source_version
                 FROM library_files WHERE file_id = ?1",
                [&file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((media_id, path, source, revision, source_version)) = reservation else {
            return Err(StoreError::Protected(
                "Transfer reservation changed or was detached".into(),
            ));
        };
        if media_id != row.media_id.to_string()
            || path != row.path
            || source.as_deref() != source_path
        {
            return Err(StoreError::Protected(
                "Transfer reservation changed or was detached".into(),
            ));
        }
        let file = self.place_ledger_row(row, Some(transfer_mode), source_path)?;
        let existing_units = self.get_file_units(row.id)?;
        let units = if !existing_units.is_empty() {
            existing_units
        } else {
            ledger_units(row)
        };
        let expected = FileVersion {
            revision,
            source_version,
        };
        self.publish_files(
            &WriteScope {
                library_id: file.library_id,
                config_revision: 0,
            },
            &[PublicationRequest {
                file: &file,
                units: &units,
                expected_version: Some(&expected),
                nfo_observation: None,
                probe_required: false,
            }],
            crate::unix_now() * 1000,
        )?;
        tracing::info!(file_id = %row.id, path = %row.path, "Finalized Transfer publication");
        Ok(())
    }

    /// Whether the path holds a reserved (not yet published) Transfer row.
    pub fn transfer_pending(&self, path: &str) -> Result<bool, StoreError> {
        let pending: Option<bool> = self
            .library
            .query_row(
                "SELECT publication_state = 'pending' FROM library_files
                 WHERE path_key = ?1 OR display_path = ?1
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
                 WHERE path_key = ?1 OR display_path = ?1
                 LIMIT 1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(actual.flatten().as_deref() == expected)
    }

    /// Build the `library_files` row for a legacy [`LedgerRow`]: the owning Library
    /// and root come from the path, the version from whatever is already recorded
    /// there (a re-import of the same path keeps one file identity).
    fn place_ledger_row(
        &self,
        row: &LedgerRow,
        transfer_mode: Option<&str>,
        source_path: Option<&str>,
    ) -> Result<LibraryFile, StoreError> {
        let media = self
            .get_media(row.media_id)?
            .ok_or_else(|| StoreError::Missing(format!("media {}", row.media_id)))?;
        let library = self
            .library_for_path_strict(Path::new(&row.path), media.kind)?
            .ok_or_else(|| {
                StoreError::Invalid(format!("path is outside every Library root: {}", row.path))
            })?;
        let root_id = self.root_for_path(&library.id, &row.path)?;
        let existing: Option<(String, i64, String)> = self
            .library
            .query_row(
                "SELECT file_id, file_revision, source_version FROM library_files
                 WHERE path_key = ?1 OR display_path = ?1
                 LIMIT 1",
                [&row.path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (file_id, revision, source_version) = match existing {
            Some((file_id, revision, source_version)) => {
                (file_id.parse::<LedgerId>()?, revision, source_version)
            }
            None => (
                row.id,
                1,
                format!("{}-{}", crate::unix_now(), row.id),
            ),
        };
        let artwork = crate::files::DiscoveredArtwork::discover(Path::new(&row.path));
        Ok(LibraryFile {
            file_id,
            library_id: library.id.parse::<LibraryId>()?,
            root_id,
            media_id: row.media_id,
            kind: media.kind,
            path: row.path.clone().into(),
            version: FileVersion {
                revision,
                source_version,
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: None,
            stat_known: false,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: source_path.map(Into::into),
            transfer_mode: transfer_mode.map(str::to_string),
            resolution: row.resolution.clone(),
            codec: row.codec.clone(),
            hdr: row.hdr.clone(),
            quality_source: row.quality_source,
            confidence: row.confidence,
            filter_score: row.filter_score,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: artwork.poster,
            backdrop_path: artwork.backdrop,
            still_path: artwork.still,
        })
    }

    /// The deepest root of `library_id` that contains `path`.
    fn root_for_path(&self, library_id: &str, path: &str) -> Result<String, StoreError> {
        let resolved = crate::library_paths::resolved_path(Path::new(path))?;
        let mut best: Option<(usize, String)> = None;
        for root in self.list_library_roots()? {
            if root.library_id.as_deref() != Some(library_id) {
                continue;
            }
            let normalized = crate::library_paths::resolved_path(&root.path)?;
            if !resolved.starts_with(&normalized) {
                continue;
            }
            let depth = normalized.components().count();
            if best.as_ref().is_none_or(|(best_depth, _)| *best_depth < depth) {
                best = Some((depth, root.id.clone()));
            }
        }
        best.map(|(_, id)| id).ok_or_else(|| {
            StoreError::Invalid(format!(
                "no root of library {library_id} contains {path}"
            ))
        })
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

/// Coverage of one legacy row: whole-work when it carries no episode, otherwise
/// the single season/episode it names.
fn ledger_units(row: &LedgerRow) -> Vec<UnitKey> {
    match (row.season, row.episode) {
        (Some(season), Some(episode)) => vec![UnitKey::new(season as i32, episode as i32)],
        _ => vec![UnitKey::WHOLE],
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use domain::{Confidence, MediaKind, QualitySource};

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
    fn ledger_for_media_batch_covers_chunk_boundary_and_dedups() {
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

        assert_eq!(store.ledger_for_media_batch(&ids).unwrap().len(), 501);
        assert!(store.ledger_for_media_batch(&[]).unwrap().is_empty());

        // 重复 id 只查询一次，不产生重复行。
        let mut duplicated = ids.clone();
        duplicated.extend(ids.iter().take(10).copied());
        assert_eq!(store.ledger_for_media_batch(&duplicated).unwrap().len(), 501);
    }
}
