//! Integrity check and repair operation for a library.
//!
//! Two-way enumeration:
//! 1. From files/coverage and playback facts to derived rows (library_items, library_units,
//!    library_item_terms, user_library_items).
//! 2. From derived rows back to raw facts (detecting reverse orphans).
//!
//! In repair mode:
//! Re-reduces affected media in write transactions from localized facts.
//! When errors are detected and repair=false, persists `integrity_state='failed'`.
//! When repair succeeds and a final verification passes, clears to `integrity_state='clean'`.

use std::collections::{BTreeSet, HashSet};

use domain::{LibraryId, MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::library_operations::roots::BatchOutcome;
use crate::library_write::reduce::reduce_library_media;
use crate::playback::types::OperationStatus;
use crate::{Store, StoreError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IntegrityPlan {
    pub repair: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub missing_items: Vec<MediaId>,
    pub missing_user_items: Vec<(UserId, MediaId)>,
    pub orphan_items: Vec<MediaId>,
    pub orphan_user_items: Vec<(UserId, MediaId)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct IntegrityCursor {
    #[serde(default)]
    pub last_media_id: Option<String>,
    #[serde(default)]
    pub issues_found: usize,
    #[serde(default)]
    pub issues_repaired: usize,
    #[serde(default)]
    pub report: IntegrityReport,
}

impl Store {
    pub fn begin_library_integrity(
        &self,
        library: LibraryId,
        repair: bool,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let lib_str = library.to_string();

        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT config_revision, lifecycle_state FROM libraries WHERE library_id = ?1",
                params![lib_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((revision, lifecycle)) = current else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        if lifecycle != "ready" && lifecycle != "reindexing" {
            return Err(StoreError::Protected(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }

        let plan = IntegrityPlan { repair };
        let plan_json =
            serde_json::to_string(&plan).map_err(|e| StoreError::Invalid(e.to_string()))?;

        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'integrity', 'planned', ?3, ?4, NULL, NULL, ?5, ?5)",
            params![operation_id, lib_str, revision, plan_json, now_ms],
        )?;

        tx.commit()?;
        Ok(OperationStatus {
            operation_id: operation_id.to_string(),
            phase: "planned".into(),
            state: "planned".into(),
            error: None,
        })
    }

    /// Read the integrity report from the operation's cursor.
    pub fn library_integrity_report(
        &self,
        operation_id: &str,
    ) -> Result<IntegrityReport, StoreError> {
        let cursor_json: Option<Option<String>> = self
            .library
            .query_row(
                "SELECT cursor_json FROM library_operations WHERE operation_id = ?1",
                params![operation_id],
                |r| r.get(0),
            )
            .optional()?;

        let Some(Some(raw)) = cursor_json else {
            return Ok(IntegrityReport::default());
        };
        if raw.trim().is_empty() {
            return Ok(IntegrityReport::default());
        }

        let state: IntegrityCursor = serde_json::from_str(&raw)
            .map_err(|e| StoreError::Invalid(format!("integrity cursor: {e}")))?;
        Ok(state.report)
    }
}

pub(crate) fn apply_integrity_batch(
    tx: &Connection,
    library: &str,
    plan: &IntegrityPlan,
    operation_id: &str,
    batch_size: usize,
    now_ms: i64,
) -> Result<OperationStatus, StoreError> {
    let mut state = read_integrity_cursor(tx, operation_id)?;

    // 1. Gather distinct media candidates in this library:
    // (a) from library_files (published)
    // (b) from library_items (derived)
    // (c) from user_library_items (derived user rows)
    let candidates = {
        let mut set = BTreeSet::new();
        let mut stmt = tx.prepare(
            "SELECT DISTINCT media_id FROM library_files WHERE library_id = ?1 AND publication_state = 'published'",
        )?;
        let rows = stmt.query_map(params![library], |r| r.get::<_, String>(0))?;
        for r in rows {
            set.insert(r?);
        }

        let mut stmt =
            tx.prepare("SELECT DISTINCT media_id FROM library_items WHERE library_id = ?1")?;
        let rows = stmt.query_map(params![library], |r| r.get::<_, String>(0))?;
        for r in rows {
            set.insert(r?);
        }

        let mut stmt =
            tx.prepare("SELECT DISTINCT media_id FROM user_library_items WHERE library_id = ?1")?;
        let rows = stmt.query_map(params![library], |r| r.get::<_, String>(0))?;
        for r in rows {
            set.insert(r?);
        }
        set
    };

    let paged: Vec<String> = match state.last_media_id.as_deref() {
        Some(last) => candidates
            .range((
                std::ops::Bound::Excluded(last.to_string()),
                std::ops::Bound::Unbounded,
            ))
            .take(batch_size)
            .cloned()
            .collect(),
        None => candidates.iter().take(batch_size).cloned().collect(),
    };

    let lib_id = library.parse::<LibraryId>()?;
    for media_str in &paged {
        let media_id = media_str.parse::<MediaId>()?;
        let issues = inspect_media_integrity(tx, library, media_id, &mut state.report)?;
        if issues > 0 {
            state.issues_found += issues;
            if plan.repair {
                // Re-reduce locally from raw facts
                reduce_library_media(tx, lib_id, media_id)?;
                state.issues_repaired += issues;
            }
        }
        state.last_media_id = Some(media_str.clone());
    }

    let total_remaining_after_page = match state.last_media_id.as_deref() {
        Some(last) => candidates
            .range((
                std::ops::Bound::Excluded(last.to_string()),
                std::ops::Bound::Unbounded,
            ))
            .count(),
        None => 0,
    };
    let has_more = total_remaining_after_page > 0;

    if has_more {
        write_integrity_cursor(tx, operation_id, &state, "applying", now_ms)?;
        Ok(OperationStatus {
            operation_id: operation_id.to_string(),
            phase: "applying".into(),
            state: "applying".into(),
            error: None,
        })
    } else {
        let remaining = check_remaining_issues(tx, library)?;
        let final_state = if !plan.repair {
            if state.issues_found > 0 || remaining > 0 {
                tx.execute(
                    "UPDATE libraries SET integrity_state = 'failed' WHERE library_id = ?1",
                    params![library],
                )?;
            } else {
                tx.execute(
                    "UPDATE libraries SET integrity_state = 'clean' WHERE library_id = ?1",
                    params![library],
                )?;
            }
            "succeeded"
        } else {
            if remaining == 0 {
                tx.execute(
                    "UPDATE libraries SET integrity_state = 'clean' WHERE library_id = ?1",
                    params![library],
                )?;
                "succeeded"
            } else {
                tx.execute(
                    "UPDATE libraries SET integrity_state = 'failed' WHERE library_id = ?1",
                    params![library],
                )?;
                "failed"
            }
        };
        write_integrity_cursor(tx, operation_id, &state, final_state, now_ms)?;
        Ok(OperationStatus {
            operation_id: operation_id.to_string(),
            phase: final_state.into(),
            state: final_state.into(),
            error: None,
        })
    }
}

fn read_integrity_cursor(
    tx: &Connection,
    operation_id: &str,
) -> Result<IntegrityCursor, StoreError> {
    let raw: Option<Option<String>> = tx
        .query_row(
            "SELECT cursor_json FROM library_operations WHERE operation_id = ?1",
            params![operation_id],
            |r| r.get(0),
        )
        .optional()?;
    match raw.flatten().filter(|s| !s.trim().is_empty()) {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Invalid(e.to_string())),
        None => Ok(IntegrityCursor::default()),
    }
}

fn write_integrity_cursor(
    tx: &Connection,
    operation_id: &str,
    cursor: &IntegrityCursor,
    state: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    let json_str = serde_json::to_string(cursor).map_err(|e| StoreError::Invalid(e.to_string()))?;
    tx.execute(
        "UPDATE library_operations SET state = ?2, cursor_json = ?3, updated_at_ms = ?4 WHERE operation_id = ?1",
        params![operation_id, state, json_str, now_ms],
    )?;
    Ok(())
}

fn inspect_media_integrity(
    tx: &Connection,
    library: &str,
    media_id: MediaId,
    report: &mut IntegrityReport,
) -> Result<usize, StoreError> {
    let mut count = 0;
    let m_str = media_id.to_string();

    let has_files: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files WHERE library_id = ?1 AND media_id = ?2 AND publication_state = 'published')",
        params![library, m_str],
        |r| r.get(0),
    )?;

    let has_item: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_items WHERE library_id = ?1 AND media_id = ?2)",
        params![library, m_str],
        |r| r.get(0),
    )?;

    if has_files && !has_item {
        report.missing_items.push(media_id);
        count += 1;
    } else if !has_files && has_item {
        report.orphan_items.push(media_id);
        count += 1;
    }

    // Check user_library_items
    let users_with_playback: HashSet<String> = {
        let mut set = HashSet::new();
        let mut stmt =
            tx.prepare("SELECT DISTINCT user_id FROM playback_units WHERE media_id = ?1")?;
        let rows = stmt.query_map(params![m_str], |r| r.get::<_, String>(0))?;
        for r in rows {
            set.insert(r?);
        }
        set
    };

    if has_files {
        for u in &users_with_playback {
            let has_user_row: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM user_library_items WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3)",
                params![u, library, m_str],
                |r| r.get(0),
            )?;
            if !has_user_row {
                if let Ok(user_id) = u.parse::<UserId>() {
                    report.missing_user_items.push((user_id, media_id));
                    count += 1;
                }
            }
        }
    }

    let users_with_summary: HashSet<String> = {
        let mut set = HashSet::new();
        let mut stmt = tx.prepare(
            "SELECT DISTINCT user_id FROM user_library_items WHERE library_id = ?1 AND media_id = ?2",
        )?;
        let rows = stmt.query_map(params![library, m_str], |r| r.get::<_, String>(0))?;
        for r in rows {
            set.insert(r?);
        }
        set
    };

    for u in &users_with_summary {
        if !has_files || !users_with_playback.contains(u) {
            if let Ok(user_id) = u.parse::<UserId>() {
                report.orphan_user_items.push((user_id, media_id));
                count += 1;
            }
        }
    }

    Ok(count)
}

fn check_remaining_issues(tx: &Connection, library: &str) -> Result<usize, StoreError> {
    // Check missing items
    let missing_items: i64 = tx.query_row(
        "SELECT COUNT(DISTINCT media_id) FROM library_files f
         WHERE library_id = ?1 AND publication_state = 'published'
           AND NOT EXISTS (SELECT 1 FROM library_items i WHERE i.library_id = f.library_id AND i.media_id = f.media_id)",
        params![library],
        |r| r.get(0),
    )?;

    // Check orphan items
    let orphan_items: i64 = tx.query_row(
        "SELECT COUNT(DISTINCT media_id) FROM library_items i
         WHERE library_id = ?1
           AND NOT EXISTS (SELECT 1 FROM library_files f WHERE f.library_id = i.library_id AND f.media_id = i.media_id AND f.publication_state = 'published')",
        params![library],
        |r| r.get(0),
    )?;

    Ok((missing_items + orphan_items) as usize)
}
