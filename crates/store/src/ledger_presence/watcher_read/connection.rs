//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn watcher_presence_page(
        &self,
        path: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LedgerPresenceObservation>, StoreError> {
        self.watcher_presence_page_with_work(path, after, limit)
            .map(|(rows, _, _)| rows)
    }

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
                read_window(self.library, "path_key = ?1", &[&key, &(limit as i64)])?;
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
                self.library,
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
