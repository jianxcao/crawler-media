use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    MediaId, MediaKind, QualitySource,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct FileCursor {
    library_id: LibraryId,
    media_id: MediaId,
    order: String,
    after_file_id: LedgerId,
}

const FILE_CURSOR_ORDER: &str = "file_id_asc";

use rusqlite::params;

use super::types::FilePage;
use super::super::{Store, StoreError};

impl Store {
    pub fn library_file_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        after: Option<&str>,
        limit: usize,
    ) -> Result<FilePage, StoreError> {
        let lib_id_str = library_id.to_string();
        let media_id_str = media_id.to_string();
        let limit = limit.clamp(1, 100);
        // Ask for one row beyond the page so the last page reports `next_cursor: null`
        // instead of pretending a following page exists.
        let probe = limit + 1;

        let decoded = match after {
            Some(raw) => {
                let cursor: FileCursor = serde_json::from_str(raw)
                    .map_err(|_| StoreError::Invalid("malformed file cursor".into()))?;
                if cursor.library_id != library_id || cursor.media_id != media_id || cursor.order != FILE_CURSOR_ORDER {
                    return Err(StoreError::Invalid("file cursor scope mismatch".into()));
                }
                Some(cursor.after_file_id.to_string())
            }
            None => None,
        };
        let (sql, params_vec): (String, Vec<rusqlite::types::Value>) = match decoded {
            Some(cursor) => (
                "SELECT file_id, library_id, root_id, media_id, kind, path_key, display_path,
                        presence_state, publication_state, file_revision, source_version,
                        size_bytes, stat_known, mtime_ns, created_ns, source_digest,
                        source_path, transfer_mode, resolution, codec, hdr, quality_source,
                        confidence, filter_score, release_quality, duration_ms, tracks_version,
                        poster_path, backdrop_path, still_path
                 FROM library_files
                 WHERE library_id = ?1 AND media_id = ?2 AND file_id > ?3
                   AND publication_state = 'published'
                 ORDER BY file_id ASC
                 LIMIT ?4"
                    .into(),
                vec![lib_id_str.into(), media_id_str.into(), cursor.into(), (probe as i64).into()],
            ),
            None => (
                "SELECT file_id, library_id, root_id, media_id, kind, path_key, display_path,
                        presence_state, publication_state, file_revision, source_version,
                        size_bytes, stat_known, mtime_ns, created_ns, source_digest,
                        source_path, transfer_mode, resolution, codec, hdr, quality_source,
                        confidence, filter_score, release_quality, duration_ms, tracks_version,
                        poster_path, backdrop_path, still_path
                 FROM library_files
                 WHERE library_id = ?1 AND media_id = ?2
                   AND publication_state = 'published'
                 ORDER BY file_id ASC
                 LIMIT ?3"
                    .into(),
                vec![lib_id_str.into(), media_id_str.into(), (probe as i64).into()],
            ),
        };

        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |r| {
            let file_id: LedgerId = r.get::<_, String>(0)?.parse().unwrap();
            let lib_id: LibraryId = r.get::<_, String>(1)?.parse().unwrap();
            let root_id: String = r.get(2)?;
            let media_id: MediaId = r.get::<_, String>(3)?.parse().unwrap();
            let kind: MediaKind = r.get::<_, String>(4)?.parse().unwrap_or(MediaKind::Movie);
            let path_key: String = r.get(5)?;
            let presence_str: String = r.get(7)?;
            let pub_str: String = r.get(8)?;
            let size_i: Option<i64> = r.get(11)?;
            let stat_k: i64 = r.get(12)?;
            let mtime: Option<i64> = r.get(13)?;
            let created: Option<i64> = r.get(14)?;
            let src_digest: Option<String> = r.get(15)?;
            let src_path: Option<String> = r.get(16)?;
            let trans_mode: Option<String> = r.get(17)?;
            let res: Option<String> = r.get(18)?;
            let codec: Option<String> = r.get(19)?;
            let hdr: Option<String> = r.get(20)?;
            let qs_str: String = r.get(21)?;
            let conf_str: String = r.get(22)?;
            let f_score: Option<i32> = r.get(23)?;
            let rel_q: Option<String> = r.get(24)?;
            let dur: Option<i64> = r.get(25)?;
            let tr_ver: i64 = r.get(26)?;
            let poster: Option<String> = r.get(27)?;
            let backdrop: Option<String> = r.get(28)?;
            let still: Option<String> = r.get(29)?;

            let presence = FilePresence::from_str(&presence_str).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    format!("unknown file presence '{presence_str}' for file_id {file_id}").into(),
                )
            })?;
            let publication = FilePublication::from_str(&pub_str).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    format!("unknown file publication '{pub_str}' for file_id {file_id}").into(),
                )
            })?;
            let quality_source = match qs_str.as_str() {
                "probe" => QualitySource::Probe,
                _ => QualitySource::Release,
            };
            let confidence = match conf_str.as_str() {
                "high" => Confidence::High,
                _ => Confidence::Low,
            };

            Ok(LibraryFile {
                file_id,
                library_id: lib_id,
                root_id,
                media_id,
                kind,
                path: path_key.into(),
                version: FileVersion {
                    revision: r.get(9)?,
                    source_version: r.get(10)?,
                },
                presence,
                publication,
                size_bytes: size_i.map(|s| s as u64),
                stat_known: stat_k != 0,
                created_ns: created,
                modified_ns: mtime,
                source_digest: src_digest,
                source_path: src_path.map(|p| p.into()),
                transfer_mode: trans_mode,
                resolution: res,
                codec,
                hdr,
                quality_source,
                confidence,
                filter_score: f_score,
                release_quality: rel_q,
                duration_ms: dur,
                tracks_version: tr_ver,
                poster_path: poster.map(|p| p.into()),
                backdrop_path: backdrop.map(|p| p.into()),
                still_path: still.map(|p| p.into()),
            })
        })?;

        let mut files = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = files.len() > limit;
        files.truncate(limit);
        let next_cursor = if has_more {
            files.last().map(|file| serde_json::to_string(&FileCursor {
                library_id,
                media_id,
                order: FILE_CURSOR_ORDER.to_string(),
                after_file_id: file.file_id,
            })).transpose().map_err(|error| StoreError::Invalid(error.to_string()))?
        } else {
            None
        };

        Ok(FilePage { files, next_cursor })
    }
}
