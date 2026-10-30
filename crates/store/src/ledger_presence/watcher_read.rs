//! Indexed path windows for watcher file and directory events.
use super::LedgerPresenceObservation;
use crate::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit};
use crate::{Store, StoreError};
use rusqlite::{Connection, ToSql};

impl Store {
    /// Read up to 100 complete file observations for the exact path or its descendants.
    /// The exclusive cursor is the `path_key` of the previous page's last file.
    pub fn watcher_presence_page(
        &self,
        path: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LedgerPresenceObservation>, StoreError> {
        self.with_library_read(|db| db.watcher_presence_page(path, after, limit))
    }

    /// The same read with SQLite work counters for bounded-work verification.
    pub fn watcher_presence_page_with_work(
        &self,
        path: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<LedgerPresenceObservation>, i64, i64), StoreError> {
        self.with_library_read(|db| db.watcher_presence_page_with_work(path, after, limit))
    }
}

fn read_window(
    conn: &Connection,
    predicate: &str,
    values: &[&dyn ToSql],
) -> Result<(Vec<LedgerPresenceObservation>, i64, i64), StoreError> {
    let limit_param = values.len();
    let sql = format!(
        "WITH page AS MATERIALIZED (
            SELECT * FROM library_files WHERE {predicate}
            ORDER BY path_key LIMIT ?{limit_param}
        ) SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode, f.missing_at
          FROM page f LEFT JOIN library_file_units u ON u.file_id=f.file_id
          ORDER BY f.path_key,u.season,u.episode"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(values)?;
    let mut facts = Vec::new();
    let mut missing = std::collections::HashMap::new();
    while let Some(row) = rows.next()? {
        let fact = map_file_unit(row)?;
        missing.insert(fact.0.file_id, row.get::<_, Option<i64>>(32)?);
        facts.push(fact);
    }
    drop(rows);
    let vm = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
    let scans = i64::from(stmt.get_status(rusqlite::StatementStatus::FullscanStep));
    let observations = group_file_coverage(facts)
        .into_iter()
        .map(|file| LedgerPresenceObservation {
            path_key: file.path_key.clone(),
            missing_at: missing.remove(&file.file.file_id).flatten(),
            presence: file.file.presence,
            file,
        })
        .collect();
    Ok((observations, vm, scans))
}

#[path = "watcher_read/connection.rs"]
mod connection;
