//! Library-scoped missing file facts and physical-file pagination.
use std::{collections::HashMap, path::PathBuf};

use domain::{LedgerId, LibraryId, MediaId, MediaKind, UnitKey};
use rusqlite::params;

use crate::{Store, StoreError};

#[derive(Clone, Debug)]
pub struct MissingFile {
    pub file_id: LedgerId,
    pub media_id: MediaId,
    pub title: String,
    pub kind: MediaKind,
    pub year: Option<u16>,
    pub path: PathBuf,
    pub missing_at: i64,
    pub units: Vec<UnitKey>,
}

fn query(after: bool) -> String {
    let seek = if after {
        "AND (missing_at,file_id)>(?3,?4)"
    } else {
        ""
    };
    format!(
        "WITH page AS MATERIALIZED (
           SELECT file_id,media_id,display_path,missing_at FROM library_files INDEXED BY files_missing
           WHERE library_id=?1 AND missing_at IS NOT NULL {seek}
           ORDER BY missing_at,file_id LIMIT ?2
         ) SELECT p.file_id,p.media_id,p.display_path,p.missing_at,m.title,m.kind,m.year,
                  u.season,u.episode
         FROM page p CROSS JOIN media m ON m.media_id=p.media_id
         LEFT JOIN library_file_units u ON u.file_id=p.file_id"
    )
}

impl Store {
    pub fn missing_files_page(
        &self,
        library: LibraryId,
        after: Option<(i64, LedgerId)>,
        limit: usize,
    ) -> Result<Vec<MissingFile>, StoreError> {
        self.with_read(crate::Db::Library, |conn| {
            let sql = query(after.is_some());
            let mut statement = conn.prepare_cached(&sql)?;
            let limit = i64::try_from(limit)
                .map_err(|_| StoreError::Invalid("missing page limit".into()))?;
            let mut rows = match after {
                Some((at, file)) => {
                    statement.query(params![library.to_string(), limit, at, file.to_string()])?
                }
                None => statement.query(params![library.to_string(), limit])?,
            };
            let mut files: Vec<MissingFile> = Vec::new();
            let mut slots = HashMap::new();
            while let Some(row) = rows.next()? {
                let file_id: LedgerId = row.get::<_, String>(0)?.parse()?;
                let slot = match slots.get(&file_id) {
                    Some(slot) => *slot,
                    None => {
                        let kind: String = row.get(5)?;
                        slots.insert(file_id, files.len());
                        files.push(MissingFile {
                            file_id,
                            media_id: row.get::<_, String>(1)?.parse()?,
                            path: row.get::<_, String>(2)?.into(),
                            missing_at: row.get(3)?,
                            title: row.get(4)?,
                            kind: kind.parse().map_err(StoreError::Invalid)?,
                            year: row.get(6)?,
                            units: Vec::new(),
                        });
                        files.len() - 1
                    }
                };
                if let Some(season) = row.get::<_, Option<i32>>(7)? {
                    files[slot].units.push(UnitKey::new(season, row.get(8)?));
                }
            }
            for file in &mut files {
                file.units
                    .sort_unstable_by_key(|unit| (unit.season, unit.episode));
                file.units.dedup();
            }
            files.sort_unstable_by(|a, b| {
                a.missing_at
                    .cmp(&b.missing_at)
                    .then_with(|| a.file_id.as_uuid().cmp(&b.file_id.as_uuid()))
            });
            Ok(files)
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn missing_files_query_plans(
        &self,
        library: LibraryId,
        after: Option<(i64, LedgerId)>,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        self.with_read(crate::Db::Library, |conn| {
            let mut statement =
                conn.prepare(&format!("EXPLAIN QUERY PLAN {}", query(after.is_some())))?;
            let read = |row: &rusqlite::Row<'_>| row.get::<_, String>(3);
            let plans = match after {
                Some((at, file)) => statement
                    .query_map(
                        params![library.to_string(), limit as i64, at, file.to_string()],
                        read,
                    )?
                    .collect::<Result<Vec<_>, _>>()?,
                None => statement
                    .query_map(params![library.to_string(), limit as i64], read)?
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(plans)
        })
    }
}
