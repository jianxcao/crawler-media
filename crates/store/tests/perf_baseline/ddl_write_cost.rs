//! The write side of plan Task 10's v3 layout: what each new index costs to maintain, and
//! whether a measured query actually uses it.
//!
//! An index is pure write amplification until a query plan chooses it, so "should this index
//! exist?" needs two numbers: the extra milliseconds it adds to a write batch, and the read
//! work that changes when it is missing. Both are measured **inside one fixture**, alternating
//! `with index` / `without index` batches and re-creating the index from its own
//! `sqlite_master` SQL between rounds. Comparing two separately seeded fixtures was tried
//! first and is useless here: on a loaded machine the fixture-to-fixture spread (hundreds of
//! ms on a 1000-file publication) is larger than any index maintenance cost.
//!
//! Run in the isolated target directory so it does not contend with other agents:
//!
//! ```bash
//! CARGO_TARGET_DIR=target/perf cargo test -p store --release --test perf_baseline \
//!   -- --ignored --nocapture write_cost
//! ```
//!
//! Everything here is wall-clock (index maintenance has no statement counter), so nothing is
//! asserted beyond "the measurement ran": the gate never depends on a duration.

use std::time::Instant;

use super::fixture::{Fixture, Scale};
use super::ops::{self, ProgressSession};

/// The three indexes the v3 layout added that still exist.
///
/// v3 added six; v5 dropped the three `playback_{sessions,logs,metrics}_media` ones
/// because their only consumer (`Store::delete_playback_for_media`) is now test-only and
/// no production path deletes playback rows by `media_id` — an index maintained on every
/// playback write that nothing read. They are out of this attribution because there is no
/// index left to measure.
pub const NEW_INDEXES: [&str; 3] = [
    "operations_library_kind",
    "pending_deletions_due",
    "metadata_inputs_media",
];

/// The five tables Task 10 converted to `WITHOUT ROWID`.
pub const WITHOUT_ROWID_TABLES: [&str; 5] = [
    "library_file_units",
    "library_item_terms",
    "library_protocol_terms",
    "library_protocol_people",
    "user_library_items",
];

/// Files published per interleaved batch, and progress events per interleaved batch.
const BATCH: usize = 100;
const EVENTS: usize = 50;
/// `with index` / `without index` rounds per index.
const ROUNDS: usize = 2;


#[test]
#[ignore = "write-cost attribution; run in --release with CARGO_TARGET_DIR=target/perf"]
fn write_cost_of_the_v3_indexes() {
    let fixture = Fixture::build(Scale::WRITE_PROBE);
    let db = fixture.store.data_dir().join("library.db");
    let sqls = index_sql(&db);
    // The fixture reserves exactly `ROUNDS * 2` batches per index, so there is no spare
    // batch to warm up with: the first measured batch pays for the cold page cache once.
    println!(
        "| index | publication ms per {BATCH} files with / without | progress ms per event with / without | read counters that changed when the index was dropped |"
    );
    println!("|---|---|---|---|");
    let mut cursor = 0;
    for index in NEW_INDEXES {
        let sql = sqls
            .iter()
            .find(|(name, _)| name == index)
            .map(|(_, sql)| sql.clone())
            .unwrap_or_else(|| panic!("{index} is not in library.db"));
        let (mut with_publication, mut without_publication) = (Vec::new(), Vec::new());
        let (mut with_progress, mut without_progress) = (Vec::new(), Vec::new());
        let mut changed: Vec<String> = Vec::new();
        for round in 0..ROUNDS {
            // The two read measurements are adjacent, on the same data, so the only
            // difference between them is the index.
            let reads_with = read_counters(&fixture);
            drop_index(&db, index);
            let reads_without = read_counters(&fixture);
            changed.extend(diff(&reads_with, &reads_without, index));
            // Alternate which side runs first so a warm-up trend cannot look like a cost.
            if round % 2 == 0 {
                without_publication.push(publish_batch(&fixture, cursor));
                cursor += 1;
                without_progress.push(progress_batch(&fixture));
                recreate_index(&db, index, &sql);
                with_publication.push(publish_batch(&fixture, cursor));
                cursor += 1;
                with_progress.push(progress_batch(&fixture));
            } else {
                recreate_index(&db, index, &sql);
                with_publication.push(publish_batch(&fixture, cursor));
                cursor += 1;
                with_progress.push(progress_batch(&fixture));
                drop_index(&db, index);
                without_publication.push(publish_batch(&fixture, cursor));
                cursor += 1;
                without_progress.push(progress_batch(&fixture));
            }
        }
        recreate_index(&db, index, &sql);
        println!(
            "| {index} | {:.1} / {:.1} | {:.3} / {:.3} | {} |",
            mean(&with_publication),
            mean(&without_publication),
            mean(&with_progress),
            mean(&without_progress),
            if changed.is_empty() {
                "none".to_string()
            } else {
                changed.join("; ")
            },
        );
    }
}

/// Names every read operation whose deterministic counters moved.
fn diff(
    with: &[(String, String, String)],
    without: &[(String, String, String)],
    index: &str,
) -> Vec<String> {
    with.iter()
        .zip(without.iter())
        .filter_map(|((label, statements, steps), (_, other_statements, other_steps))| {
            if statements != other_statements || steps != other_steps {
                Some(format!(
                    "{label}: {statements}/{steps} → {other_statements}/{other_steps} ({index} dropped)"
                ))
            } else {
                None
            }
        })
        .collect()
}

/// `(label, statements, full_scan_steps)` for every idempotent read operation.
fn read_counters(fixture: &Fixture) -> Vec<(String, String, String)> {
    ops::reads_without_layout_guard(fixture)
        .into_iter()
        .map(|report| {
            (
                report.label,
                counter(report.work.statements),
                counter(report.work.fullscan_steps),
            )
        })
        .collect()
}

fn counter(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "n/a".into(),
    }
}

/// Publishes the next `BATCH` reserved slots; returns the wall clock in milliseconds.
fn publish_batch(fixture: &Fixture, batch: usize) -> f64 {
    let started = Instant::now();
    ops::publish_slot_range(fixture, batch * BATCH, BATCH);
    started.elapsed().as_secs_f64() * 1_000.
}

/// Drives `EVENTS` progress events of one session; returns the per-event milliseconds.
fn progress_batch(fixture: &Fixture) -> f64 {
    let mut session = ProgressSession::open(fixture);
    let started = Instant::now();
    for _ in 0..EVENTS {
        session.commit_next(&fixture.store);
    }
    started.elapsed().as_secs_f64() * 1_000. / EVENTS as f64
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// On-disk size per index and per table, so the audit doc can name what the layout costs at
/// this scale (`dbstat`, one row per b-tree).
#[test]
#[ignore = "layout size report; run in --release with CARGO_TARGET_DIR=target/perf"]
fn layout_size_report() {
    let fixture = Fixture::build(Scale::REALISTIC);
    let db = rusqlite::Connection::open(fixture.store.data_dir().join("library.db")).unwrap();
    println!("| name | kind | bytes | pages |");
    println!("|---|---|---|---|");
    let mut statement = db
        .prepare(
            "SELECT name, SUM(pgsize), COUNT(*) FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC",
        )
        .unwrap();
    let rows: Vec<(String, i64, i64)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (name, bytes, pages) in rows {
        let kind = if WITHOUT_ROWID_TABLES.contains(&name.as_str()) {
            "table (v3 WITHOUT ROWID)"
        } else if NEW_INDEXES.contains(&name.as_str()) {
            "index (new in v3)"
        } else {
            "-"
        };
        println!("| {name} | {kind} | {bytes} | {pages} |");
    }
}

fn index_sql(path: &std::path::Path) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open(path).unwrap();
    let mut statement = db
        .prepare("SELECT name, sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn drop_index(path: &std::path::Path, index: &str) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(&format!("DROP INDEX \"{index}\""))
        .unwrap_or_else(|error| panic!("dropping {index}: {error}"));
}

fn recreate_index(path: &std::path::Path, index: &str, sql: &str) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(sql)
        .unwrap_or_else(|error| panic!("recreating {index}: {error}"));
}
