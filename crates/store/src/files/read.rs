use domain::{LedgerId, LibraryId, MediaId};
use rusqlite::{OptionalExtension, params};

use super::types::FileDetails;
use crate::{Store, StoreError};

impl Store {
    pub fn get_file_path_and_library(&self, file_id: LedgerId) -> Result<Option<(std::path::PathBuf, String)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path_key, library_id FROM library_files WHERE file_id = ?1",
        )?;
        let row = stmt.query_row([file_id.to_string()], |r: &rusqlite::Row<'_>| {
            let path_str: String = r.get(0)?;
            let lib_id: String = r.get(1)?;
            Ok((std::path::PathBuf::from(path_str), lib_id))
        }).optional()?;
        Ok(row)
    }

    pub fn item_resolution_and_seasons(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        preferred_file_id: Option<LedgerId>,
    ) -> (String, Vec<i32>) {
        let res = match preferred_file_id {
            Some(fid) => self.library.query_row(
                "SELECT resolution FROM library_files WHERE file_id = ?1",
                params![fid.to_string()],
                |r: &rusqlite::Row<'_>| r.get::<_, Option<String>>(0),
            ).unwrap_or(None).unwrap_or_else(|| "1080p".into()),
            None => "1080p".into(),
        };

        let seasons = (|| -> Result<Vec<i32>, rusqlite::Error> {
            let mut stmt = self.library.prepare(
                "SELECT DISTINCT season FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season >= 0 ORDER BY season ASC",
            )?;
            let rows = stmt.query_map(params![library_id.to_string(), media_id.to_string()], |r: &rusqlite::Row<'_>| {
                r.get::<_, i32>(0)
            })?;
            let mut list = Vec::new();
            for r in rows {
                list.push(r?);
            }
            Ok(list)
        })().unwrap_or_default();

        (res, seasons)
    }

    /// Probe details of a whole page of files in one query. A detail view must not read the
    /// `file_details` row per file, and it must not fall back to the filesystem either.
    pub fn file_details_batch(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<std::collections::HashMap<LedgerId, FileDetails>, StoreError> {
        let mut details = std::collections::HashMap::new();
        if file_ids.is_empty() {
            return Ok(details);
        }
        let placeholders = (0..file_ids.len())
            .map(|index| format!("?{}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<rusqlite::types::Value> = file_ids
            .iter()
            .map(|id| id.to_string().into())
            .collect();
        let sql = format!(
            "SELECT file_id, source_revision, source_version, tracks_version, video_json,
                    audio_json, subtitle_json, chapters_json, fingerprint_json,
                    outro_fingerprint_json, format_duration_ms
             FROM file_details WHERE file_id IN ({placeholders})"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
            let file_id: String = row.get(0)?;
            Ok((
                file_id,
                FileDetails {
                    source_revision: row.get(1)?,
                    source_version: row.get(2)?,
                    tracks_version: row.get(3)?,
                    video_json: row.get(4)?,
                    audio_json: row.get(5)?,
                    subtitle_json: row.get(6)?,
                    chapters_json: row.get(7)?,
                    fingerprint_json: row.get(8)?,
                    outro_fingerprint_json: row.get(9)?,
                    format_duration_ms: row.get(10)?,
                },
            ))
        })?;
        for row in rows {
            let (file_id, details_row) = row?;
            let Ok(file_id) = file_id.parse::<LedgerId>() else {
                continue;
            };
            details.insert(file_id, details_row);
        }
        Ok(details)
    }

    pub fn missing_paths_for_library(&self, library_id: &str) -> Result<std::collections::HashMap<String, i64>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path_key, COALESCE(missing_at, 0) FROM library_files WHERE library_id = ?1 AND presence_state = 'missing'",
        )?;
        let rows = stmt.query_map([library_id], |r: &rusqlite::Row<'_>| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (p, m) = r?;
            map.insert(p, m);
        }
        Ok(map)
    }
}
