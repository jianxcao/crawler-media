//! Bounded coverage windows by explicit recorded ownership.
use super::LedgerPresenceObservation;
use crate::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId};
use rusqlite::params;

impl Store {
    /// At most 100 files, with complete coverage and missing flags captured together.
    /// The cursor is exclusive and refers to file identity, never joined unit rows.
    pub fn ledger_presence_page(
        &self,
        library_id: LibraryId,
        media_id: Option<MediaId>,
        after: Option<LedgerId>,
        limit: usize,
    ) -> Result<Vec<LedgerPresenceObservation>, StoreError> {
        self.ledger_presence_page_with_work(library_id, media_id, after, limit)
            .map(|(rows, _, _)| rows)
    }

    /// Same production read plus SQLite VM and full-scan steps for workload assertions.
    pub fn ledger_presence_page_with_work(
        &self,
        library_id: LibraryId,
        media_id: Option<MediaId>,
        after: Option<LedgerId>,
        limit: usize,
    ) -> Result<(Vec<LedgerPresenceObservation>, i64, i64), StoreError> {
        if !(1..=100).contains(&limit) {
            return Err(StoreError::Invalid(
                "presence page limit must be 1..=100".into(),
            ));
        }
        let media_filter = if media_id.is_some() {
            "AND media_id = ?4"
        } else {
            ""
        };
        let sql = format!(
            "WITH page AS MATERIALIZED (
                SELECT * FROM library_files WHERE library_id=?1 AND file_id>?2 {media_filter}
                ORDER BY file_id LIMIT ?3
             ) SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode, f.missing_at
               FROM page f LEFT JOIN library_file_units u ON u.file_id=f.file_id
               ORDER BY f.file_id,u.season,u.episode"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let lib = library_id.to_string();
        let cursor = after.map(|id| id.to_string()).unwrap_or_default();
        let media = media_id.map(|id| id.to_string());
        let mut rows = if let Some(media) = media.as_ref() {
            stmt.query(params![lib, cursor, limit as i64, media])?
        } else {
            stmt.query(params![lib, cursor, limit as i64])?
        };
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
}
