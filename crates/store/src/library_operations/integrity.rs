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

use std::collections::HashSet;

use domain::{LibraryId, MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

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
    /// Media whose derived rows (items, units, terms, user rows) differ from what their
    /// raw facts reduce to. Capped at [`REPORT_CAP`]; `issues_found` keeps the true total.
    #[serde(default)]
    pub mismatched_media: Vec<MediaId>,
    /// Exact issue counts per category; the sample lists above are capped.
    #[serde(default)]
    pub totals: IntegrityTotals,
    /// Whether any sample list dropped entries because it reached [`REPORT_CAP`].
    #[serde(default)]
    pub truncated: bool,
}

/// Exact per-category counts behind the capped report samples.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityTotals {
    pub missing_items: usize,
    pub missing_user_items: usize,
    pub orphan_items: usize,
    pub orphan_user_items: usize,
    pub mismatched_media: usize,
}

/// Append a sample while it stays under [`REPORT_CAP`]; count it regardless.
fn sample<T>(list: &mut Vec<T>, total: &mut usize, truncated: &mut bool, value: T) {
    *total += 1;
    if list.len() < REPORT_CAP {
        list.push(value);
    } else {
        *truncated = true;
    }
}

/// Most Media a persisted report lists; the cursor never accumulates the whole Library.
pub const REPORT_CAP: usize = 100;

impl IntegrityReport {
    /// Whether the check found any issue (the bounded issue lists are non-empty).
    pub fn has_issues(&self) -> bool {
        !(self.missing_items.is_empty()
            && self.missing_user_items.is_empty()
            && self.orphan_items.is_empty()
            && self.orphan_user_items.is_empty()
            && self.mismatched_media.is_empty())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct IntegrityCursor {
    #[serde(default)]
    pub last_media_id: Option<String>,
    #[serde(default)]
    pub issues_found: usize,
    #[serde(default)]
    pub issues_repaired: usize,
    /// Media still inconsistent after their repair was re-verified.
    #[serde(default)]
    pub unrepaired: usize,
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
        let tx = crate::write_tx(&self.library)?;
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

    // One keyset page over every Media the Library has a raw or derived row for; the
    // page is bounded by `batch_size`, never a full-Library materialisation (S02).
    let (paged, has_more) =
        next_media_page(tx, library, state.last_media_id.as_deref(), batch_size)?;

    let lib_id = library.parse::<LibraryId>()?;
    for media_str in &paged {
        let media_id = media_str.parse::<MediaId>()?;
        let probe = probe_media(tx, lib_id, library, media_str)?;
        let mut issues = record_presence_issues(&probe, media_id, &mut state.report);
        if probe.drift() {
            let report = &mut state.report;
            sample(
                &mut report.mismatched_media,
                &mut report.totals.mismatched_media,
                &mut report.truncated,
                media_id,
            );
            issues = issues.max(1);
        }
        if issues > 0 {
            state.issues_found += issues;
            if plan.repair {
                reduce_library_media(tx, lib_id, media_id)?;
                // Re-verify with the same expected set the check used.
                if probe_media(tx, lib_id, library, media_str)?.drift() {
                    state.unrepaired += 1;
                } else {
                    state.issues_repaired += issues;
                }
            }
        }
        state.last_media_id = Some(media_str.clone());
    }

    if has_more {
        write_integrity_cursor(tx, operation_id, &state, "applying", now_ms)?;
        Ok(OperationStatus {
            operation_id: operation_id.to_string(),
            phase: "applying".into(),
            state: "applying".into(),
            error: None,
        })
    } else {
        // Every Media was compared (and, in repair mode, re-verified) page by page.
        let clean = if plan.repair {
            state.unrepaired == 0
        } else {
            state.issues_found == 0
        };
        tx.execute(
            "UPDATE libraries SET integrity_state = ?2 WHERE library_id = ?1",
            params![library, if clean { "clean" } else { "failed" }],
        )?;
        let final_state = if plan.repair && !clean {
            "failed"
        } else {
            "succeeded"
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

/// Actual derived rows of one Media next to what its raw facts reduce to.
///
/// The expected side is the scoped reduction itself (run in a rolled-back
/// savepoint), so a Library that owns none of a user's seasons/episodes
/// legally has no user row, and detection, repair and re-verification share
/// one definition of "expected".
struct MediaProbe {
    actual: Vec<String>,
    expected: Vec<String>,
    actual_item: bool,
    expected_item: bool,
    actual_users: HashSet<String>,
    expected_users: HashSet<String>,
}

impl MediaProbe {
    fn drift(&self) -> bool {
        self.actual != self.expected
    }
}

fn probe_media(
    tx: &Connection,
    lib_id: LibraryId,
    library: &str,
    media: &str,
) -> Result<MediaProbe, StoreError> {
    let actual = derived_snapshot(tx, library, media)?;
    let actual_item = has_item(tx, library, media)?;
    let actual_users = summary_users(tx, library, media)?;
    tx.execute_batch("SAVEPOINT integrity_probe")?;
    let reduced = reduce_library_media(tx, lib_id, media.parse()?).and_then(|_| {
        Ok((
            derived_snapshot(tx, library, media)?,
            has_item(tx, library, media)?,
            summary_users(tx, library, media)?,
        ))
    });
    tx.execute_batch("ROLLBACK TO integrity_probe; RELEASE integrity_probe")?;
    let (expected, expected_item, expected_users) = reduced?;
    Ok(MediaProbe {
        actual,
        expected,
        actual_item,
        expected_item,
        actual_users,
        expected_users,
    })
}

fn has_item(tx: &Connection, library: &str, media: &str) -> Result<bool, StoreError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_items WHERE library_id = ?1 AND media_id = ?2)",
        params![library, media],
        |r| r.get(0),
    )?)
}

fn summary_users(
    tx: &Connection,
    library: &str,
    media: &str,
) -> Result<HashSet<String>, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT user_id FROM user_library_items WHERE library_id = ?1 AND media_id = ?2",
    )?;
    let rows = stmt.query_map(params![library, media], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Missing/orphan item and user rows, by comparing actual with expected.
fn record_presence_issues(
    probe: &MediaProbe,
    media_id: MediaId,
    report: &mut IntegrityReport,
) -> usize {
    let mut count = 0;
    if probe.expected_item && !probe.actual_item {
        sample(
            &mut report.missing_items,
            &mut report.totals.missing_items,
            &mut report.truncated,
            media_id,
        );
        count += 1;
    } else if !probe.expected_item && probe.actual_item {
        sample(
            &mut report.orphan_items,
            &mut report.totals.orphan_items,
            &mut report.truncated,
            media_id,
        );
        count += 1;
    }
    let mut missing: Vec<_> = probe
        .expected_users
        .difference(&probe.actual_users)
        .collect();
    let mut orphan: Vec<_> = probe
        .actual_users
        .difference(&probe.expected_users)
        .collect();
    missing.sort();
    orphan.sort();
    for user in missing {
        if let Ok(user_id) = user.parse::<UserId>() {
            sample(
                &mut report.missing_user_items,
                &mut report.totals.missing_user_items,
                &mut report.truncated,
                (user_id, media_id),
            );
            count += 1;
        }
    }
    for user in orphan {
        if let Ok(user_id) = user.parse::<UserId>() {
            sample(
                &mut report.orphan_user_items,
                &mut report.totals.orphan_user_items,
                &mut report.truncated,
                (user_id, media_id),
            );
            count += 1;
        }
    }
    count
}

/// One page of Media ids (after `after`) with any raw or derived row in the Library,
/// plus whether another page follows.
fn next_media_page(
    tx: &Connection,
    library: &str,
    after: Option<&str>,
    batch: usize,
) -> Result<(Vec<String>, bool), StoreError> {
    let mut stmt = tx.prepare(
        // Each arm walks its `(library_id, media_id, ...)` index from the cursor and stops
        // after one page, so a page costs O(batch) rather than a whole-Library union.
        "SELECT media_id FROM (
             SELECT * FROM (SELECT DISTINCT media_id FROM library_files
              WHERE library_id = ?1 AND media_id > coalesce(?2, '')
                AND publication_state = 'published'
              ORDER BY media_id LIMIT ?3)
             UNION SELECT * FROM (SELECT DISTINCT media_id FROM library_items
              WHERE library_id = ?1 AND media_id > coalesce(?2, '') ORDER BY media_id LIMIT ?3)
             UNION SELECT * FROM (SELECT DISTINCT media_id FROM library_units
              WHERE library_id = ?1 AND media_id > coalesce(?2, '') ORDER BY media_id LIMIT ?3)
             UNION SELECT * FROM (SELECT DISTINCT media_id FROM library_item_terms
              WHERE library_id = ?1 AND media_id > coalesce(?2, '') ORDER BY media_id LIMIT ?3)
             UNION SELECT * FROM (SELECT DISTINCT media_id FROM user_library_items
              WHERE library_id = ?1 AND media_id > coalesce(?2, '') ORDER BY media_id LIMIT ?3))
         ORDER BY media_id LIMIT ?3",
    )?;
    let mut ids = stmt
        .query_map(params![library, after, (batch + 1) as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let more = ids.len() > batch;
    ids.truncate(batch);
    Ok((ids, more))
}

const DERIVED_TABLES: [&str; 4] = [
    "library_items",
    "library_units",
    "library_item_terms",
    "user_library_items",
];

fn derived_snapshot(
    tx: &Connection,
    library: &str,
    media: &str,
) -> Result<Vec<String>, StoreError> {
    let mut rows = Vec::new();
    for table in DERIVED_TABLES {
        let columns: Vec<String> = {
            let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
            stmt.query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|c| !c.ends_with("_revision"))
                .collect()
        };
        let list = columns.join(", ");
        let mut stmt = tx.prepare(&format!(
            "SELECT {list} FROM {table} WHERE library_id = ?1 AND media_id = ?2 ORDER BY {list}"
        ))?;
        let width = columns.len();
        let mapped = stmt.query_map(params![library, media], |r| {
            let mut values = Vec::with_capacity(width);
            for i in 0..width {
                values.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
            }
            Ok(format!("{table}:{}", values.join("|")))
        })?;
        for row in mapped {
            rows.push(row?);
        }
    }
    Ok(rows)
}
