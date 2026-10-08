use std::path::PathBuf;

use domain::{Confidence, MediaKind};
use rusqlite::{OptionalExtension, params};

pub(super) const CURRENT_TRACKS_VERSION: i64 = 1;
const SETTING_TRANSFER_MODE: &str = "transfer_mode";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMediaMarker {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub episode: u32,
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub source: String,
    pub locked: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct MarkerResultReplacement {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub markers: Vec<StoredMediaMarker>,
    pub chapter_updates: Vec<(String, Vec<library::ChapterMarker>)>,
}

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
        let path: String = self.app.query_row(
            "SELECT r.path FROM library_roots r
             JOIN libraries l ON l.id = r.library_id
             WHERE l.kind = ?1 AND l.is_default = 1
             ORDER BY r.sort_order, r.path LIMIT 1",
            params![kind.as_str()],
            |row| row.get(0),
        )?;
        Ok(PathBuf::from(path))
    }

    /// Replace the primary root of the kind's default library.
    pub fn set_library_root(&self, kind: MediaKind, path: &str) -> Result<(), StoreError> {
        let root_id: String = self.app.query_row(
            "SELECT r.id FROM library_roots r
             JOIN libraries l ON l.id = r.library_id
             WHERE l.kind = ?1 AND l.is_default = 1
             ORDER BY r.sort_order, r.path LIMIT 1",
            params![kind.as_str()],
            |row| row.get(0),
        )?;
        self.app.execute(
            "UPDATE library_roots SET path = ?2 WHERE id = ?1",
            params![root_id, path],
        )?;
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

    pub fn get_file_meta(&self, ledger_id: &str) -> Result<Option<library::Tracks>, StoreError> {
        self.library
            .query_row(
                "SELECT audio_json, subtitle_json, video_json, tracks_version FROM file_meta WHERE ledger_id = ?1",
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
                    let audio = serde_json::from_str::<Vec<library::AudioTrack>>(&row.get::<_, String>(0)?)
                        .unwrap_or_default();
                    let subtitles =
                        serde_json::from_str::<Vec<library::SubtitleTrack>>(&row.get::<_, String>(1)?)
                            .unwrap_or_default();
                    let video = video_raw
                        .and_then(|s| serde_json::from_str::<library::VideoTrack>(&s).ok());
                    Ok(Some(library::Tracks { video, audio, subtitles }))
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
        self.library.execute(
            "INSERT INTO file_meta (
                 ledger_id, audio_json, subtitle_json, video_json, tracks_version,
                 source_version, format_duration_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(ledger_id) DO UPDATE SET
               audio_json = excluded.audio_json,
               subtitle_json = excluded.subtitle_json,
               video_json = excluded.video_json,
               tracks_version = excluded.tracks_version,
               source_version = excluded.source_version,
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
        Ok(())
    }

    pub fn get_cached_chapters(
        &self,
        ledger_id: &str,
    ) -> Result<Option<Vec<library::ChapterMarker>>, StoreError> {
        self.library
            .query_row(
                "SELECT chapters_json FROM file_meta WHERE ledger_id = ?1",
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
        let json_str = serde_json::to_string(chapters)?;
        self.library.execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
             VALUES (?1, '[]', '[]', ?2)
             ON CONFLICT(ledger_id) DO UPDATE SET chapters_json = excluded.chapters_json",
            params![ledger_id, json_str],
        )?;
        Ok(())
    }

    pub fn clear_cached_chapters(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE file_meta SET chapters_json = NULL WHERE ledger_id = ?1",
            params![ledger_id],
        )?;
        Ok(())
    }

    /// 读取单集声纹指纹缓存（探测任务写入；未探测/失败为 None）。
    pub fn get_fingerprint(&self, ledger_id: &str) -> Result<Option<Vec<u32>>, StoreError> {
        self.library
            .query_row(
                "SELECT fingerprint_json FROM file_meta WHERE ledger_id = ?1",
                params![ledger_id],
                |row| {
                    let raw: Option<String> = row.get(0).ok().flatten();
                    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
                },
            )
            .optional()
            .map(|opt| opt.flatten())
            .map_err(Into::into)
    }

    /// 写入单集声纹指纹缓存；None 表示清除（内容变化后失效）。
    pub fn put_fingerprint(
        &self,
        ledger_id: &str,
        fingerprint: Option<&[u32]>,
    ) -> Result<(), StoreError> {
        let json_str =
            fingerprint.map(|fp| serde_json::to_string(fp).unwrap_or_else(|_| "[]".into()));
        self.library.execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, fingerprint_json)
             VALUES (?1, '[]', '[]', ?2)
             ON CONFLICT(ledger_id) DO UPDATE SET fingerprint_json = excluded.fingerprint_json",
            params![ledger_id, json_str],
        )?;
        Ok(())
    }

    /// 读取单集片尾声纹指纹缓存。
    pub fn get_outro_fingerprint(&self, ledger_id: &str) -> Result<Option<Vec<u32>>, StoreError> {
        self.library
            .query_row(
                "SELECT outro_fingerprint_json FROM file_meta WHERE ledger_id = ?1",
                params![ledger_id],
                |row| {
                    let raw: Option<String> = row.get(0).ok().flatten();
                    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
                },
            )
            .optional()
            .map(|opt| opt.flatten())
            .map_err(Into::into)
    }

    /// 写入单集片尾声纹指纹缓存。
    pub fn put_outro_fingerprint(
        &self,
        ledger_id: &str,
        fingerprint: Option<&[u32]>,
    ) -> Result<(), StoreError> {
        let json_str =
            fingerprint.map(|fp| serde_json::to_string(fp).unwrap_or_else(|_| "[]".into()));
        self.library.execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, outro_fingerprint_json)
             VALUES (?1, '[]', '[]', ?2)
             ON CONFLICT(ledger_id) DO UPDATE SET outro_fingerprint_json = excluded.outro_fingerprint_json",
            params![ledger_id, json_str],
        )?;
        Ok(())
    }

    pub fn delete_media_marker(
        &self,
        media_id: domain::MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<(), StoreError> {
        let season = season.unwrap_or(1);
        let episode = episode.unwrap_or(1);
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![media_id.to_string(), season, episode],
        )?;
        Ok(())
    }

    pub fn delete_media_markers_for_media(
        &self,
        media_id: domain::MediaId,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1",
            params![media_id.to_string()],
        )?;
        Ok(())
    }

    /// Replace a season's marker rows and chapter caches in one transaction.
    /// Existing results remain readable until this transaction commits.
    pub fn replace_marker_results(
        &self,
        media_id: domain::MediaId,
        season: u32,
        markers: &[StoredMediaMarker],
        chapter_updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch(&[MarkerResultReplacement {
            media_id,
            season,
            markers: markers.to_vec(),
            chapter_updates: chapter_updates.to_vec(),
        }])
    }

    /// Atomically replace marker rows and chapter caches across one or more seasons.
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None)
    }

    /// Commit all season results and the marker-refresh terminal state together.
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id))
    }

    fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let serialized = replacements
            .iter()
            .flat_map(|replacement| {
                replacement
                    .chapter_updates
                    .iter()
                    .map(|(ledger_id, chapters)| {
                        Ok((ledger_id.clone(), serde_json::to_string(chapters)?))
                    })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let tx = self.library.unchecked_transaction()?;
        for replacement in replacements {
            tx.execute(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2",
                params![replacement.media_id.to_string(), replacement.season],
            )?;
            for marker in &replacement.markers {
                tx.execute(
                    "INSERT INTO media_markers (
                         media_id, season, episode, intro_start_ms, intro_end_ms,
                         outro_start_ms, outro_end_ms, source, locked, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        marker.media_id.to_string(),
                        marker.season,
                        marker.episode,
                        marker.intro_start_ms,
                        marker.intro_end_ms,
                        marker.outro_start_ms,
                        marker.outro_end_ms,
                        marker.source,
                        marker.locked,
                        marker.updated_at
                    ],
                )?;
            }
        }
        for (ledger_id, chapters_json) in serialized {
            tx.execute(
                "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
                 VALUES (?1, '[]', '[]', ?2)
                 ON CONFLICT(ledger_id) DO UPDATE SET chapters_json = excluded.chapters_json",
                params![ledger_id, chapters_json],
            )?;
        }
        if let Some(job_id) = completed_job_id {
            let finished_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or_default();
            let changed = tx.execute(
                "UPDATE probe_jobs SET status = 'succeeded', finished_at_ms = ?2
                 WHERE id = ?1 AND status IN ('queued', 'running')
                   AND completed = total AND failed = 0",
                params![job_id, finished_at_ms],
            )?;
            if changed == 0 {
                return Err(StoreError::Missing(format!(
                    "active completed marker refresh job {job_id}"
                )));
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_file_meta_by_ledger_id(&self, ledger_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM file_meta WHERE ledger_id = ?1",
            params![ledger_id],
        )?;
        self.delete_fingerprint_cache(ledger_id)?;
        Ok(())
    }

    pub fn get_media_marker(
        &self,
        media_id: domain::MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<StoredMediaMarker>, StoreError> {
        let season = season.unwrap_or(1);
        let episode = episode.unwrap_or(1);
        self.library
            .query_row(
                "SELECT intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
                 FROM media_markers
                 WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![media_id.to_string(), season, episode],
                |row| {
                    Ok(StoredMediaMarker {
                        media_id,
                        season,
                        episode,
                        intro_start_ms: row.get(0)?,
                        intro_end_ms: row.get(1)?,
                        outro_start_ms: row.get(2)?,
                        outro_end_ms: row.get(3)?,
                        source: row.get(4)?,
                        locked: row.get::<_, i64>(5)? != 0,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn put_media_marker(&self, marker: &StoredMediaMarker) -> Result<(), StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.library.execute(
            "INSERT INTO media_markers (
                media_id, season, episode, intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                intro_start_ms = excluded.intro_start_ms,
                intro_end_ms = excluded.intro_end_ms,
                outro_start_ms = excluded.outro_start_ms,
                outro_end_ms = excluded.outro_end_ms,
                source = excluded.source,
                locked = excluded.locked,
                updated_at = excluded.updated_at
             WHERE media_markers.locked = 0 OR excluded.locked = 1",
            params![
                marker.media_id.to_string(),
                marker.season,
                marker.episode,
                marker.intro_start_ms,
                marker.intro_end_ms,
                marker.outro_start_ms,
                marker.outro_end_ms,
                marker.source,
                if marker.locked { 1 } else { 0 },
                now,
            ],
        )?;
        Ok(())
    }

    pub fn insert_unidentified(
        &self,
        path: String,
        confidence: Confidence,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO unidentified (path, confidence) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET confidence = excluded.confidence",
            params![path, confidence.as_str()],
        )?;
        Ok(())
    }

    pub fn list_unidentified(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path, confidence FROM unidentified ORDER BY path")?;
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

#[cfg(test)]
mod marker_replacement_tests {
    use super::{MarkerResultReplacement, Store, StoredMediaMarker};

    fn marker(
        media_id: domain::MediaId,
        season: u32,
        source: &str,
        start_ms: i64,
    ) -> StoredMediaMarker {
        StoredMediaMarker {
            media_id,
            season,
            episode: 1,
            intro_start_ms: Some(start_ms),
            intro_end_ms: Some(start_ms + 60_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: source.into(),
            locked: false,
            updated_at: 1,
        }
    }

    #[test]
    fn multi_season_refresh_keeps_old_results_on_failure_and_completes_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let media_id = domain::MediaId::new();
        let job_id = "multi-season-refresh";
        let old_chapters = vec![library::ChapterMarker {
            start_ms: 0,
            end_ms: 300_000,
            title: Some("原章节".into()),
            marker_type: None,
            synthetic: false,
        }];
        for season in [1, 2] {
            store
                .put_media_marker(&marker(media_id, season, "old", season as i64 * 100_000))
                .unwrap();
            store
                .put_cached_chapters(&format!("ledger-s{season}"), &old_chapters)
                .unwrap();
        }
        let specs = [
            super::super::probe_tasks::ProbeJobUnitSpec {
                ledger_id: "ledger-s1",
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
            },
            super::super::probe_tasks::ProbeJobUnitSpec {
                ledger_id: "ledger-s2",
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
            },
        ];
        assert!(
            store
                .create_probe_job(
                    job_id,
                    "marker_refresh",
                    &media_id.to_string(),
                    None,
                    "marker-refresh-test",
                    &specs,
                )
                .unwrap()
        );
        for ledger_id in ["ledger-s1", "ledger-s2"] {
            assert!(store.start_probe_unit(job_id, ledger_id).unwrap());
            store
                .finish_probe_unit(job_id, ledger_id, true, None)
                .unwrap();
        }
        let s1_new = marker(media_id, 1, "new", 111_000);
        let s2_duplicate = marker(media_id, 2, "new", 222_000);
        let replacements = [
            MarkerResultReplacement {
                media_id,
                season: 1,
                markers: vec![s1_new],
                chapter_updates: vec![(
                    "ledger-s1".into(),
                    vec![library::ChapterMarker {
                        start_ms: 111_000,
                        end_ms: 171_000,
                        title: Some("新片头".into()),
                        marker_type: Some(library::MarkerType::IntroStart),
                        synthetic: false,
                    }],
                )],
            },
            MarkerResultReplacement {
                media_id,
                season: 2,
                markers: vec![s2_duplicate.clone(), s2_duplicate],
                chapter_updates: vec![("ledger-s2".into(), Vec::new())],
            },
        ];

        assert!(
            store
                .complete_marker_refresh(job_id, &replacements)
                .is_err()
        );
        assert_eq!(
            store.get_probe_job(job_id).unwrap().unwrap().status,
            "running"
        );
        for season in [1, 2] {
            assert_eq!(
                store
                    .get_media_marker(media_id, Some(season), Some(1))
                    .unwrap()
                    .unwrap()
                    .source,
                "old"
            );
            assert_eq!(
                store
                    .get_cached_chapters(&format!("ledger-s{season}"))
                    .unwrap()
                    .unwrap(),
                old_chapters
            );
        }

        let valid_replacements = [
            MarkerResultReplacement {
                media_id,
                season: 1,
                markers: vec![marker(media_id, 1, "new", 111_000)],
                chapter_updates: vec![("ledger-s1".into(), old_chapters.clone())],
            },
            MarkerResultReplacement {
                media_id,
                season: 2,
                markers: vec![marker(media_id, 2, "new", 222_000)],
                chapter_updates: vec![("ledger-s2".into(), old_chapters.clone())],
            },
        ];
        store
            .complete_marker_refresh(job_id, &valid_replacements)
            .unwrap();
        assert_eq!(
            store.get_probe_job(job_id).unwrap().unwrap().status,
            "succeeded"
        );
        for (season, expected_source) in [(1, "new"), (2, "new")] {
            assert_eq!(
                store
                    .get_media_marker(media_id, Some(season), Some(1))
                    .unwrap()
                    .unwrap()
                    .source,
                expected_source
            );
        }
    }
}

impl Store {
    /// 全库文件核验：路径不存在的 ledger 行标 missing_at=now，恢复的文件清掉标记。
    pub fn verify_library_files(
        &self,
        roots: &[std::path::PathBuf],
        now: i64,
    ) -> Result<(i64, i64), StoreError> {
        let rows = self.list_ledger()?;
        let mut missing = 0;
        let mut restored = 0;
        for row in rows {
            let in_root = roots
                .iter()
                .any(|root| std::path::Path::new(&row.path).starts_with(root));
            if !in_root {
                continue;
            }
            if std::path::Path::new(&row.path).exists() {
                let n = self.library.execute(
                    "UPDATE ledger SET missing_at = NULL WHERE id = ?1 AND missing_at IS NOT NULL",
                    params![row.id.to_string()],
                )?;
                if n > 0 {
                    restored += 1;
                }
            } else {
                let n = self.library.execute(
                    "UPDATE ledger SET missing_at = ?1 WHERE id = ?2 AND missing_at IS NULL",
                    params![now, row.id.to_string()],
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
        let mut stmt = self
            .library
            .prepare("SELECT path, missing_at FROM ledger WHERE missing_at IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 删除缺失文件对应的 ledger 行（仅限这些根目录内）。
    pub fn delete_missing_rows(&self, roots: &[std::path::PathBuf]) -> Result<usize, StoreError> {
        let missing = self.missing_at_by_path()?;
        let mut deleted = 0;
        for (path, _) in missing {
            let in_root = roots
                .iter()
                .any(|root| std::path::Path::new(&path).starts_with(root));
            if in_root {
                deleted += self
                    .library
                    .execute("DELETE FROM ledger WHERE path = ?1", params![path])?;
            }
        }
        Ok(deleted)
    }

    /// 删除指定媒体的缺失文件对应 ledger 行（仅限这些根目录内）。
    pub fn delete_missing_rows_for_media(
        &self,
        roots: &[std::path::PathBuf],
        media_id: domain::MediaId,
    ) -> Result<usize, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path FROM ledger WHERE media_id = ?1 AND missing_at IS NOT NULL")?;
        let paths = stmt.query_map(params![media_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut deleted = 0;
        for path in paths {
            let path = path?;
            let in_root = roots
                .iter()
                .any(|root| std::path::Path::new(&path).starts_with(root));
            if in_root {
                deleted += self
                    .library
                    .execute("DELETE FROM ledger WHERE path = ?1", params![path])?;
            }
        }
        Ok(deleted)
    }

    /// 该 Media 是否有「文件缺失」的 ledger 行（transfer 据此决定是否重链）。
    pub fn media_has_missing(&self, media_id: domain::MediaId) -> Result<bool, StoreError> {
        Ok(self
            .library
            .query_row(
                "SELECT 1 FROM ledger WHERE media_id = ?1 AND missing_at IS NOT NULL LIMIT 1",
                params![media_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// 文件重新入库后清除缺失标记。
    pub fn clear_ledger_missing(&self, path: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE ledger SET missing_at = NULL WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }
}
