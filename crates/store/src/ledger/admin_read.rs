//! Complete management ledger joins stored owner, media and coverage facts.
use std::collections::HashMap;

use crate::{
    Store, StoreError,
    maps::{LIBRARY_FILE_COLUMNS, map_file_unit},
};
use domain::{FileCoverage, LedgerId, MediaKind};
use rusqlite::Connection;

#[derive(Clone, Debug)]
pub struct LedgerRow {
    pub coverage: FileCoverage,
    pub media_title: String,
    pub media_kind: MediaKind,
}

impl Store {
    /// Complete management facts from one Library snapshot; no filesystem reads.
    pub fn ledger_rows(&self) -> Result<Vec<LedgerRow>, StoreError> {
        self.with_read(crate::Db::Library, read)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn ledger_rows_query_plan(&self) -> Result<Vec<String>, StoreError> {
        self.with_read(crate::Db::Library, |conn| {
            let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {}", query()))?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()?)
        })
    }
}

fn query() -> String {
    format!(
        "SELECT {LIBRARY_FILE_COLUMNS},u.season,u.episode,m.title,m.kind \
         FROM library_files f CROSS JOIN media m ON m.media_id=f.media_id \
         LEFT JOIN library_file_units u ON u.file_id=f.file_id"
    )
}

fn read(conn: &Connection) -> Result<Vec<LedgerRow>, StoreError> {
    let mut statement = conn.prepare_cached(&query())?;
    let mut facts = statement.query([])?;
    let mut slots: HashMap<LedgerId, usize> = HashMap::new();
    let mut rows: Vec<LedgerRow> = Vec::new();
    while let Some(row) = facts.next()? {
        let (file, path_key, unit) = map_file_unit(row)?;
        let slot = match slots.get(&file.file_id) {
            Some(slot) => *slot,
            None => {
                let kind: String = row.get(33)?;
                let media_kind = kind.parse().map_err(StoreError::Invalid)?;
                slots.insert(file.file_id, rows.len());
                rows.push(LedgerRow {
                    coverage: FileCoverage {
                        file,
                        path_key,
                        units: Vec::new(),
                    },
                    media_title: row.get(32)?,
                    media_kind,
                });
                rows.len() - 1
            }
        };
        if let Some(unit) = unit {
            rows[slot].coverage.units.push(unit);
        }
    }
    for row in &mut rows {
        row.coverage
            .units
            .sort_unstable_by_key(|unit| (unit.season, unit.episode));
        row.coverage.units.dedup();
    }
    Ok(rows)
}
