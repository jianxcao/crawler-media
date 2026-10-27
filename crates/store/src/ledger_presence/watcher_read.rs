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
        self.watcher_presence_page_with_work(path, after, limit)
            .map(|(rows, _, _)| rows)
    }

    /// The same read with SQLite work counters for bounded-work verification.
    pub fn watcher_presence_page_with_work(
        &self,
        path: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<LedgerPresenceObservation>, i64, i64), StoreError> {
        if !(1..=100).contains(&limit) {
            return Err(StoreError::Invalid(
                "watcher presence page limit must be 1..=100".into(),
            ));
        }
        let key = self.recorded_path_key(path);
        let mut result = Vec::new();
        let mut vm = 0;
        let mut scans = 0;
        if after.is_none() {
            let (rows, steps, fullscans) =
                read_window(&self.library, "path_key = ?1", &[&key, &(limit as i64)])?;
            result = rows;
            vm += steps;
            scans += fullscans;
        }
        if result.len() < limit {
            let prefix = if key == "/" {
                key.clone()
            } else {
                format!("{key}/")
            };
            let upper = crate::path_key::prefix_upper_bound(&prefix);
            let cursor = after.unwrap_or(&key);
            let remaining = (limit - result.len()) as i64;
            let (rows, steps, fullscans) = read_window(
                &self.library,
                "path_key >= ?1 AND path_key < ?2 AND path_key > ?3",
                &[&prefix, &upper, &cursor, &remaining],
            )?;
            result.extend(rows);
            vm += steps;
            scans += fullscans;
        }
        Ok((result, vm, scans))
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
