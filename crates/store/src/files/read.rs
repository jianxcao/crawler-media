use domain::{LedgerId, LibraryId, MediaId};
use rusqlite::{OptionalExtension, params};
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
