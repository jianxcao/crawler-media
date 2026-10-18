use std::collections::HashSet;
use domain::{LedgerId, LibraryFile, LibraryId, MediaId, MediaKind, UnitKey};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::files::LibraryItemSummary;
use super::super::{Store, StoreError};
use super::reduce::reduce_library_media;
use super::{FilePublication, PublishReport, WriteScope};

impl Store {
    pub fn publish_files(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        if files.is_empty() {
            return Ok(PublishReport::default());
        }
        if files.len() > 100 {
            return Err(StoreError::Invalid("exceeded 100 files per publication batch".into()));
        }

        let tx = self.library.unchecked_transaction()?;

        // Verify library exists and matches scope
        let lib_id_str = scope.library_id.to_string();
        let (_lib_kind, lib_rev): (String, i64) = tx
            .query_row(
                "SELECT kind, config_revision FROM libraries WHERE library_id = ?1",
                params![lib_id_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing(format!("library {}", scope.library_id)))?;

        if scope.config_revision != 0 && scope.config_revision != lib_rev {
            return Err(StoreError::Protected(format!(
                "library config revision mismatch: expected {}, current {}",
                scope.config_revision, lib_rev
            )));
        }

        let mut report = PublishReport::default();
        let mut affected_medias = HashSet::new();

        for pub_item in files {
            let f = pub_item.file;
            if f.library_id != scope.library_id {
                return Err(StoreError::Invalid("file library_id does not match write scope".into()));
            }

            let file_id_str = f.file_id.to_string();
            let media_id_str = f.media_id.to_string();
            let path_key = f.path.to_string_lossy().to_string();
            let display_path = path_key.clone();

            // Check if file already exists
            let existing: Option<(i64, String)> = tx
                .query_row(
                    "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1",
                    params![file_id_str],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;

            match existing {
                Some((cur_rev, cur_source_ver)) => {
                    if let Some(exp) = pub_item.expected_version {
                        if exp.revision != cur_rev || exp.source_version != cur_source_ver {
                            return Err(StoreError::Protected(format!(
                                "StaleVersion for file {file_id_str}: expected r{}v{}, got r{}v{}",
                                exp.revision, exp.source_version, cur_rev, cur_source_ver
                            )));
                        }
                    }

                    // Update existing
                    tx.execute(
                        "UPDATE library_files SET
                            presence_state = ?1,
                            publication_state = ?2,
                            file_revision = file_revision + 1,
                            source_version = ?3,
                            size_bytes = ?4,
                            stat_known = ?5,
                            mtime_ns = ?6,
                            created_ns = ?7,
                            source_digest = ?8,
                            resolution = ?9,
                            codec = ?10,
                            hdr = ?11,
                            quality_source = ?12,
                            confidence = ?13,
                            filter_score = ?14,
                            release_quality = ?15,
                            duration_ms = ?16,
                            tracks_version = ?17
                         WHERE file_id = ?18",
                        params![
                            f.presence.as_str(),
                            f.publication.as_str(),
                            f.version.source_version,
                            f.size_bytes.map(|s| s as i64),
                            if f.stat_known { 1 } else { 0 },
                            f.modified_ns,
                            f.created_ns,
                            f.source_digest,
                            f.resolution,
                            f.codec,
                            f.hdr,
                            f.quality_source.as_str(),
                            f.confidence.as_str(),
                            f.filter_score,
                            f.release_quality,
                            f.duration_ms,
                            f.tracks_version,
                            file_id_str,
                        ],
                    )?;
                    report.updated += 1;
                }
                None => {
                    // Insert new file
                    tx.execute(
                        "INSERT INTO library_files
                            (file_id, library_id, root_id, media_id, kind, path_key, display_path,
                             presence_state, publication_state, file_revision, source_version,
                             size_bytes, stat_known, mtime_ns, created_ns, source_digest,
                             resolution, codec, hdr, quality_source, confidence, filter_score,
                             release_quality, duration_ms, tracks_version)
                         VALUES
                            (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                             ?8, ?9, ?10, ?11,
                             ?12, ?13, ?14, ?15, ?16,
                             ?17, ?18, ?19, ?20, ?21, ?22,
                             ?23, ?24, ?25)",
                        params![
                            file_id_str,
                            lib_id_str,
                            f.root_id,
                            media_id_str,
                            f.kind.as_str(),
                            path_key,
                            display_path,
                            f.presence.as_str(),
                            f.publication.as_str(),
                            f.version.revision,
                            f.version.source_version,
                            f.size_bytes.map(|s| s as i64),
                            if f.stat_known { 1 } else { 0 },
                            f.modified_ns,
                            f.created_ns,
                            f.source_digest,
                            f.resolution,
                            f.codec,
                            f.hdr,
                            f.quality_source.as_str(),
                            f.confidence.as_str(),
                            f.filter_score,
                            f.release_quality,
                            f.duration_ms,
                            f.tracks_version,
                        ],
                    )?;
                    report.inserted += 1;
                }
            }

            // Sync units
            tx.execute(
                "DELETE FROM library_file_units WHERE file_id = ?1",
                params![file_id_str],
            )?;

            for u in pub_item.units {
                tx.execute(
                    "INSERT INTO library_file_units (file_id, library_id, media_id, season, episode)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![file_id_str, lib_id_str, media_id_str, u.season, u.episode],
                )?;
            }

            if pub_item.probe_required {
                tx.execute(
                    "INSERT INTO file_probe_obligations (file_id, created_at_ms, reason)
                     VALUES (?1, ?2, 'publish')
                     ON CONFLICT(file_id) DO NOTHING",
                    params![file_id_str, now_ms],
                )?;
            }

            affected_medias.insert(f.media_id);
        }

        // Reduce for each affected media
        for media_id in affected_medias {
            reduce_library_media(&tx, scope.library_id, media_id)?;
        }

        tx.commit()?;
        Ok(report)
    }

    pub fn get_library_item(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Option<LibraryItemSummary>, StoreError> {
        let sql = "SELECT library_id, media_id, kind, title, file_count, available_file_count,
                          missing_file_count, unknown_file_count, distinct_episode_count,
                          distinct_season_count, preferred_file_id, size_bytes
                   FROM library_items
                   WHERE library_id = ?1 AND media_id = ?2";
        let mut stmt = self.library.prepare(sql)?;
        let row = stmt
            .query_row(params![library_id.to_string(), media_id.to_string()], |r| {
                let pref_str: Option<String> = r.get(10)?;
                let pref_id = pref_str.and_then(|s| s.parse::<LedgerId>().ok());
                let size_i: Option<i64> = r.get(11)?;
                let kind_str: String = r.get(2)?;
                let kind = kind_str.parse::<MediaKind>().unwrap_or(MediaKind::Movie);

                Ok(LibraryItemSummary {
                    library_id,
                    media_id,
                    kind,
                    title: r.get(3)?,
                    file_count: r.get::<_, i64>(4)? as usize,
                    available_file_count: r.get::<_, i64>(5)? as usize,
                    missing_file_count: r.get::<_, i64>(6)? as usize,
                    unknown_file_count: r.get::<_, i64>(7)? as usize,
                    distinct_episode_count: r.get::<_, i64>(8)? as usize,
                    distinct_season_count: r.get::<_, i64>(9)? as usize,
                    preferred_file_id: pref_id,
                    size_bytes: size_i.map(|s| s as u64),
                })
            })
            .optional()?;
        Ok(row)
    }
    pub fn count_probe_obligations(&self) -> Result<usize, StoreError> {
        let count: i64 = self.library.query_row(
            "SELECT COUNT(*) FROM file_probe_obligations",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn apply_presence(
        &self,
        scope: &WriteScope,
        observations: &[super::PresenceObservation],
        mode: super::PresenceMode,
        now_ms: i64,
    ) -> Result<super::PresenceReport, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let mut report = super::PresenceReport::default();
        let mut affected_medias = HashSet::new();

        for obs in observations {
            let file_id_str = obs.file_id.to_string();
            let existing: Option<(String, String, i64, String)> = tx
                .query_row(
                    "SELECT media_id, presence_state, file_revision, source_version
                     FROM library_files
                     WHERE file_id = ?1 AND library_id = ?2",
                    params![file_id_str, scope.library_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;

            let Some((media_id_str, current_state, _rev, _src_ver)) = existing else {
                report.skipped += 1;
                continue;
            };

            let media_id = media_id_str.parse::<MediaId>().unwrap();
            match mode {
                super::PresenceMode::Verify => {
                    if current_state != obs.presence.as_str() {
                        let missing_at = if obs.presence == domain::FilePresence::Missing {
                            Some(now_ms)
                        } else {
                            None
                        };
                        tx.execute(
                            "UPDATE library_files
                             SET presence_state = ?1, missing_at = ?2, file_revision = file_revision + 1
                             WHERE file_id = ?3",
                            params![obs.presence.as_str(), missing_at, file_id_str],
                        )?;
                        if obs.presence == domain::FilePresence::Missing {
                            report.missing += 1;
                        } else if obs.presence == domain::FilePresence::Present {
                            report.restored += 1;
                        }
                        affected_medias.insert(media_id);
                    } else {
                        report.skipped += 1;
                    }
                }
                super::PresenceMode::Prune => {
                    if obs.presence == domain::FilePresence::Missing {
                        tx.execute("DELETE FROM library_files WHERE file_id = ?1", params![file_id_str])?;
                        report.deleted += 1;
                        affected_medias.insert(media_id);
                    } else {
                        report.skipped += 1;
                    }
                }
            }
        }

        for media_id in affected_medias {
            reduce_library_media(&tx, scope.library_id, media_id)?;
        }

        tx.commit()?;
        Ok(report)
    }

    pub fn publish_file_details(
        &self,
        file_id: LedgerId,
        expected: &domain::FileVersion,
        details: &super::super::files::FileDetails,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let file_id_str = file_id.to_string();

        let cur: Option<(i64, String)> = tx
            .query_row(
                "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1",
                params![file_id_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((cur_rev, cur_src_ver)) = cur else {
            return Err(StoreError::Missing(format!("file {file_id_str}")));
        };

        if cur_rev != expected.revision || cur_src_ver != expected.source_version {
            return Err(StoreError::Protected(format!(
                "StaleVersion for file {file_id_str}: expected r{}v{}, got r{}v{}",
                expected.revision, expected.source_version, cur_rev, cur_src_ver
            )));
        }

        tx.execute(
            "INSERT INTO file_details
                (file_id, source_revision, source_version, tracks_version, video_json, audio_json,
                 subtitle_json, chapters_json, fingerprint_json, outro_fingerprint_json, format_duration_ms)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(file_id) DO UPDATE SET
                 source_revision = excluded.source_revision,
                 source_version = excluded.source_version,
                 tracks_version = excluded.tracks_version,
                 video_json = excluded.video_json,
                 audio_json = excluded.audio_json,
                 subtitle_json = excluded.subtitle_json,
                 chapters_json = excluded.chapters_json,
                 fingerprint_json = excluded.fingerprint_json,
                 outro_fingerprint_json = excluded.outro_fingerprint_json,
                 format_duration_ms = excluded.format_duration_ms",
            params![
                file_id_str,
                details.source_revision,
                details.source_version,
                details.tracks_version,
                details.video_json,
                details.audio_json,
                details.subtitle_json,
                details.chapters_json,
                details.fingerprint_json,
                details.outro_fingerprint_json,
                details.format_duration_ms,
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn get_file_details(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<super::super::files::FileDetails>, StoreError> {
        let file_id_str = file_id.to_string();
        let sql = "SELECT source_revision, source_version, tracks_version, video_json, audio_json,
                          subtitle_json, chapters_json, fingerprint_json, outro_fingerprint_json,
                          format_duration_ms
                   FROM file_details
                   WHERE file_id = ?1";
        let mut stmt = self.library.prepare(sql)?;
        let res = stmt
            .query_row(params![file_id_str], |r| {
                Ok(super::super::files::FileDetails {
                    source_revision: r.get(0)?,
                    source_version: r.get(1)?,
                    tracks_version: r.get(2)?,
                    video_json: r.get(3)?,
                    audio_json: r.get(4)?,
                    subtitle_json: r.get(5)?,
                    chapters_json: r.get(6)?,
                    fingerprint_json: r.get(7)?,
                    outro_fingerprint_json: r.get(8)?,
                    format_duration_ms: r.get(9)?,
                })
            })
            .optional()?;
        Ok(res)
    }
}
