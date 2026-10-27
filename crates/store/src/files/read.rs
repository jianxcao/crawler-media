use domain::LedgerId;
use rusqlite::OptionalExtension;

use super::types::FileDetails;
use crate::{Store, StoreError};

impl Store {
    pub fn get_file_path_and_library(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<(std::path::PathBuf, String)>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path_key, library_id FROM library_files WHERE file_id = ?1")?;
        let row = stmt
            .query_row([file_id.to_string()], |r: &rusqlite::Row<'_>| {
                let path_str: String = r.get(0)?;
                let lib_id: String = r.get(1)?;
                Ok((std::path::PathBuf::from(path_str), lib_id))
            })
            .optional()?;
        Ok(row)
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
        let params: Vec<rusqlite::types::Value> =
            file_ids.iter().map(|id| id.to_string().into()).collect();
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

    /// Whether presence reconciliation has confirmed this ledger file missing.
    pub fn ledger_is_missing(&self, file_id: &str) -> Result<bool, StoreError> {
        self.library
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM library_files WHERE file_id = ?1 AND missing_at IS NOT NULL)",
                [file_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}
