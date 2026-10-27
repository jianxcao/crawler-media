//! Retention for the durable operation journals.
//!
//! `library_operations`, `scan_operations`, `media_operations` and `library_removals` are
//! append-only: every finished scan, reconfiguration, merge and removal leaves a row behind
//! forever. The browse-page and item-detail integrity read takes the *latest*
//! `library_operations` row of a Library through `operations_library_kind`, so the head of
//! that table is a hot read path and unbounded growth eventually costs the whole database.
//!
//! One batch = one `BEGIN IMMEDIATE` transaction that removes at most `batch` rows **in
//! total** across the four journals, oldest first. The candidate window is expressed in SQL
//! with the terminal predicate each owning module exports, so the window always advances and
//! a journal full of live rows can never starve the others.
//!
//! Terminal is decided by the owning module, never here:
//! - [`crate::library_operations`]: `succeeded`, or `failed` for a kind that cannot resume.
//! - [`crate::scan_operations`]: every status [`crate::ScanOperationStatus::is_terminal`].
//! - [`crate::media_operations`]: `succeeded` (a `failed` merge resumes from its cursor).
//! - [`crate::library_deletion`]: `succeeded` (any other state is still owned by the driver).
//!
//! Child rows are deleted explicitly in the same transaction. `library_removal_roots` and
//! `library_removal_files` reference `library_removals` **without** `ON DELETE CASCADE`, so
//! the parent delete would fail (NO ACTION) with `foreign_keys = ON`; `scan_seen` and
//! `library_query_count_rebuild` carry no usable cascade either. Explicit child deletes keep
//! the outcome identical whether or not a given connection enabled the pragma.

use rusqlite::{Connection, params};

use crate::{Store, StoreError};

/// Hard ceiling for one batch, whatever the caller asks for.
pub const MAX_BATCH: usize = 500;

/// What one [`Store::prune_terminal_operations`] call did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneCounts {
    /// Terminal rows the window selected (≤ `batch`).
    pub examined: usize,
    /// Rows actually removed; always `0` for a dry run.
    pub deleted: usize,
    /// Removed rows per journal, in execution order (empty for a dry run).
    pub tables: Vec<(&'static str, usize)>,
}

impl PruneCounts {
    /// The window was full, so older rows may still sit beyond it.
    pub fn window_full(&self, batch: usize) -> bool {
        self.examined >= batch
    }
}

/// One append-only operation journal and the child tables that must follow it.
struct Journal {
    table: &'static str,
    id_column: &'static str,
    /// Column the retention window compares against: when the operation last moved.
    at_column: &'static str,
    /// Child tables and the column that points at the parent id, deleted first in the same
    /// transaction. The column name is per child, not per journal: `scan_operations.id`
    /// owns `scan_seen.operation_id`.
    children: &'static [(&'static str, &'static str)],
    /// Terminal-state SQL from the module that owns the state machine.
    terminal: String,
}

impl Journal {
    fn owning_terminal(table: &'static str) -> Self {
        match table {
            "library_operations" => Self {
                table,
                id_column: "operation_id",
                at_column: "updated_at_ms",
                // `library_query_count_rebuild` has ON DELETE CASCADE, but deleting it
                // explicitly keeps the batch identical on a connection without the pragma.
                children: &[("library_query_count_rebuild", "operation_id")],
                terminal: crate::library_operations::terminal_operation_predicate(),
            },
            "scan_operations" => Self {
                table,
                id_column: "id",
                // A scan never updates its own row timestamp, so the enqueue time is the
                // only durable one. A scan older than the window is pruned even if it ran
                // long; the window is 30 days by default.
                at_column: "created_at_ms",
                children: &[("scan_seen", "operation_id")],
                terminal: crate::scan_operations::terminal_status_predicate(),
            },
            "media_operations" => Self {
                table,
                id_column: "operation_id",
                at_column: "updated_at_ms",
                children: &[],
                terminal: crate::media_operations::terminal_operation_predicate(),
            },
            other => unreachable!("unknown operation journal {other}"),
        }
    }
}

/// Every journal, in the order a batch reports them.
///
/// Note: `library_removals` is intentionally excluded from pruning: `library_removal_roots`
/// serves as the permanent security boundary (crates/store/src/library_deletion/mod.rs)
/// preventing re-ingestion of deleted library roots by watchers or auto_resolve.
const JOURNALS: [&str; 3] = [
    "library_operations",
    "scan_operations",
    "media_operations",
];

/// One row the window selected, before it is deleted.
struct Candidate {
    at_ms: i64,
    /// Index into [`JOURNALS`]: keeps the order stable when two rows share a timestamp.
    journal: usize,
    id: String,
}

impl Store {
    /// Drop terminal operation rows whose last update is older than `cutoff_ms`.
    ///
    /// `cutoff_ms` is epoch milliseconds; the comparison is strictly older (`<`), so a row
    /// exactly at the boundary survives. `batch` bounds the rows one call may remove across
    /// every journal. `dry_run` selects and counts the same window without writing, so a
    /// preview can never disagree with the next real batch.
    pub fn prune_terminal_operations(
        &self,
        cutoff_ms: i64,
        batch: usize,
        dry_run: bool,
    ) -> Result<PruneCounts, StoreError> {
        if batch == 0 {
            return Err(StoreError::Invalid(
                "operation retention batch must be >= 1".into(),
            ));
        }
        let batch = batch.min(MAX_BATCH);
        if dry_run {
            // Read connection: a preview must not even take the write lock.
            let candidates = select_candidates(&self.library, cutoff_ms, batch)?;
            return Ok(PruneCounts {
                examined: candidates.len(),
                deleted: 0,
                tables: Vec::new(),
            });
        }
        let tx = crate::write_tx(&self.library)?;
        // Select inside the same IMMEDIATE transaction that deletes: another connection
        // cannot slip a row into the window between selection and deletion.
        let candidates = select_candidates(&tx, cutoff_ms, batch)?;
        let mut counts = PruneCounts {
            examined: candidates.len(),
            deleted: 0,
            tables: Vec::new(),
        };
        for candidate in &candidates {
            let journal = Journal::owning_terminal(JOURNALS[candidate.journal]);
            if delete_candidate(&tx, &journal, &candidate.id)? {
                counts.deleted += 1;
                match counts
                    .tables
                    .iter_mut()
                    .find(|(table, _)| *table == journal.table)
                {
                    Some((_, deleted)) => *deleted += 1,
                    None => counts.tables.push((journal.table, 1)),
                }
            }
        }
        tx.commit()?;
        if counts.deleted > 0 {
            tracing::debug!(
                cutoff_ms,
                examined = counts.examined,
                deleted = counts.deleted,
                tables = ?counts.tables,
                "已清理超龄的终态 operation 行"
            );
        }
        Ok(counts)
    }
}

/// Oldest terminal rows across every journal, capped at `batch` in total.
fn select_candidates(
    conn: &Connection,
    cutoff_ms: i64,
    batch: usize,
) -> Result<Vec<Candidate>, StoreError> {
    let mut candidates = Vec::new();
    for (index, table) in JOURNALS.iter().enumerate() {
        let journal = Journal::owning_terminal(table);
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE ({}) AND {} < ?1
             ORDER BY {}, {} LIMIT ?2",
            journal.id_column,
            journal.at_column,
            journal.table,
            journal.terminal,
            journal.at_column,
            journal.at_column,
            journal.id_column,
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params![cutoff_ms, batch as i64], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                at_ms: row.get(1)?,
                journal: index,
            })
        })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    // Oldest first across journals, so a batch always retires the globally oldest rows and
    // one busy journal cannot monopolise the budget.
    candidates.sort_by(|left, right| {
        (left.at_ms, left.journal, &left.id).cmp(&(right.at_ms, right.journal, &right.id))
    });
    candidates.truncate(batch);
    Ok(candidates)
}

/// Delete one candidate and its children; `false` when the row was already gone.
fn delete_candidate(conn: &Connection, journal: &Journal, id: &str) -> Result<bool, StoreError> {
    for (child, column) in journal.children {
        conn.execute(&format!("DELETE FROM {child} WHERE {column} = ?1"), [id])?;
    }
    let deleted = conn.execute(
        &format!(
            "DELETE FROM {} WHERE {} = ?1",
            journal.table, journal.id_column
        ),
        [id],
    )?;
    Ok(deleted > 0)
}
