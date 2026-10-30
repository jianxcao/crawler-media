//! Optimistic validation of the selected page, using primary-key batch lookups.
use crate::{LibraryReadContext, LibraryReadError, Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId, ProtocolItemRef, TypedItemId};
use rusqlite::Connection;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct PageSnapshot {
    pub config_revision: i64,
    pub files: Vec<(LedgerId, i64, MediaId, LibraryId)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageValidation {
    Fresh,
    Stale { gone: Vec<LedgerId> },
}

const FILES_SQL: &str = "SELECT json_extract(j.value,'$[0]') FROM json_each(?1) j
 LEFT JOIN library_files f ON f.file_id=json_extract(j.value,'$[0]')
 LEFT JOIN media m ON m.media_id=f.media_id
 LEFT JOIN libraries l ON l.library_id=f.library_id
 WHERE f.file_id IS NULL OR m.media_id IS NULL OR l.library_id IS NULL
 OR f.file_revision<>json_extract(j.value,'$[1]')
 OR f.media_id<>json_extract(j.value,'$[2]')
 OR f.library_id<>json_extract(j.value,'$[3]')
 OR f.publication_state<>'published' OR f.presence_state<>'present'
 OR l.lifecycle_state<>'ready'
 OR EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=f.library_id)";
const COVERAGE_SQL: &str = "SELECT json_extract(j.value,'$[0]') FROM json_each(?1) j
 WHERE NOT EXISTS(SELECT 1 FROM library_file_units u
 WHERE u.file_id=json_extract(j.value,'$[0]')
 AND (json_extract(j.value,'$[1]') IS NULL
 OR (u.season=json_extract(j.value,'$[1]') AND u.episode=json_extract(j.value,'$[2]'))))";

pub(crate) fn config_revision(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row(
        "SELECT revision FROM library_query_config WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
pub(crate) fn validate_on(
    conn: &Connection,
    snapshot: &PageSnapshot,
) -> Result<PageValidation, StoreError> {
    let encoded = serde_json::to_string(&snapshot.files)?;
    let mut stmt = conn.prepare(FILES_SQL)?;
    let gone = stmt
        .query_map([encoded], |r| r.get::<_, String>(0))?
        .map(|row| Ok(row?.parse()?))
        .collect::<Result<HashSet<LedgerId>, StoreError>>()?;
    let mut seen = HashSet::new();
    let gone = snapshot
        .files
        .iter()
        .map(|row| row.0)
        .filter(|id| gone.contains(id) && seen.insert(*id))
        .collect::<Vec<_>>();
    if gone.is_empty() && config_revision(conn)? == snapshot.config_revision {
        Ok(PageValidation::Fresh)
    } else {
        Ok(PageValidation::Stale { gone })
    }
}

impl Store {
    pub fn validate_page_snapshot(
        &self,
        snapshot: &PageSnapshot,
    ) -> Result<PageValidation, StoreError> {
        self.with_library_read(|db| db.validate_page_snapshot(snapshot))
    }

    /// Credentials retain their typed failure semantics. Selected sources and coverage
    /// becoming stale are returned as data so callers can retry or remove those entries.
    pub fn validate_protocol_page(
        &self,
        contexts: &[LibraryReadContext],
        references: &[ProtocolItemRef],
        snapshot: &PageSnapshot,
    ) -> Result<PageValidation, LibraryReadError> {
        let context = contexts.first().ok_or_else(|| {
            StoreError::Invalid("protocol response missing authorization context".into())
        })?;
        for context in contexts {
            self.validate_library_read(context)?;
        }
        let authorized = context.authorized();
        let mut coverage = Vec::with_capacity(references.len());
        let mut selected = snapshot.clone();
        let mut selected_files: HashSet<_> = selected.files.iter().copied().collect();
        for reference in references {
            if !authorized.contains(&reference.library_id) {
                return Err(LibraryReadError::Unauthorized);
            }
            let source = reference.preferred_source.as_ref().ok_or_else(|| {
                StoreError::Invalid("protocol reference missing selected source".into())
            })?;
            let coords = match reference.item {
                TypedItemId::Unit { key, .. } => (Some(key.season), Some(key.episode)),
                _ => (None, None),
            };
            coverage.push((source.file_id, coords.0, coords.1));
            let file = (
                source.file_id,
                source.revision,
                reference.item.media_id(),
                reference.library_id,
            );
            if selected_files.insert(file) {
                selected.files.push(file);
            }
        }
        let validated = self.with_read(crate::Db::Library, |conn| {
            let result = validate_on(conn, &selected)?;
            let mut gone = match result {
                PageValidation::Fresh => Vec::new(),
                PageValidation::Stale { gone } => gone,
            };
            let encoded = serde_json::to_string(&coverage).map_err(StoreError::from)?;
            let mut stmt = conn.prepare(COVERAGE_SQL).map_err(StoreError::from)?;
            for row in stmt
                .query_map([encoded], |r| r.get::<_, String>(0))
                .map_err(StoreError::from)?
            {
                let id = row
                    .map_err(StoreError::from)?
                    .parse::<LedgerId>()
                    .map_err(StoreError::from)?;
                if !gone.contains(&id) {
                    gone.push(id);
                }
            }
            drop(stmt);
            let fresh = gone.is_empty() && config_revision(conn)? == snapshot.config_revision;

            Ok(if fresh {
                PageValidation::Fresh
            } else {
                PageValidation::Stale { gone }
            })
        })?;
        for context in contexts {
            self.validate_library_read(context)?;
        }
        Ok(validated)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) const VALIDATION_PLANS: &[&str] = &[FILES_SQL, COVERAGE_SQL];

#[path = "page_validate/connection.rs"]
mod connection;
