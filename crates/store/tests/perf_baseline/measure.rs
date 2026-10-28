//! The measurement harness: wall-clock percentiles for humans, deterministic SQL counters
//! for the gate.
//!
//! One rule shapes this module: **only deterministic metrics may be asserted.** Wall-clock
//! numbers are printed and recorded in `docs/audits/2026-10-26-perf-baseline.md`, never
//! compared with a threshold, because a slow CI box would then fail a healthy tree.
//!
//! Determinism is not assumed, it is enforced: every measured iteration must report exactly
//! the same [`Work`] as the first one, otherwise the gate would be comparing noise and the
//! harness panics. That assertion is why `Work` is `PartialEq` and why a probe whose SQL
//! path depends on the data (a first-touch lazy rebuild, for example) must be warmed up
//! before it is measured.

use std::time::Instant;

use store::Store;
use store::library_query::items::QueryWorkStats;

/// Deterministic work of one operation.
///
/// `statements`/`fullscan_steps`/`vm_steps` come from the store's own SQLite trace, through
/// one of three entry points: `Store::with_stats` (the uniform seam — it wraps the production
/// call in a closure, see `crates/store/src/stats.rs`), the older `*_with_stats` one-offs
/// (`library_item_page_with_stats`, `library_facets_with_stats`) and
/// `LibraryReadLease::execute_observed`. `Work::unobserved` is left for operations genuinely
/// without a counter (today: none of the measured ones — §7 of the audit doc lists what is
/// still unmeasured and why).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub statements: Option<u64>,
    pub fullscan_steps: Option<u64>,
    pub vm_steps: Option<u64>,
    pub sort_operations: Option<u64>,
    pub sql_rows: Option<u64>,
    /// Times `Store::list_file_coverage` executed its unfiltered coverage statement.
    pub ledger_scans: usize,
    /// Times `Store::file_coverage_for_media` executed its indexed read.
    pub media_ledger_reads: usize,
    /// Times a role was looked up in app.db (one per authorization decision).
    pub role_lookups: usize,
}

impl Work {
    pub fn from_stats(stats: &QueryWorkStats) -> Self {
        Self {
            statements: Some(stats.statements),
            fullscan_steps: Some(stats.fullscan_steps),
            vm_steps: Some(stats.vm_steps),
            sort_operations: Some(stats.sort_operations),
            sql_rows: Some(stats.sql_rows),
            ..Default::default()
        }
    }

    /// An operation the store does not instrument. Only the diagnostic counters apply.
    #[allow(dead_code)]
    pub fn unobserved() -> Self {
        Self::default()
    }
}

/// One measured operation: percentiles for humans, counters for the gate.
#[derive(Clone, Debug)]
pub struct Report {
    pub label: String,
    pub iterations: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub work: Work,
}

/// Runs `op` `warmups + iterations` times and reports the percentiles of the measured runs.
///
/// The closure returns the [`Work`] it observed (use [`Work::unobserved`] when the store
/// exposes no counter) and may assert on its own result; the harness adds the store-level
/// diagnostic counters around each call.
pub fn measure(
    store: &Store,
    label: &str,
    warmups: usize,
    iterations: usize,
    mut op: impl FnMut() -> Work,
) -> Report {
    assert!(
        iterations > 0,
        "{label}: need at least one measured iteration"
    );
    for _ in 0..warmups {
        let _ = op();
    }
    let mut values = Vec::with_capacity(iterations);
    let mut reference: Option<Work> = None;
    let mut last = Work::default();
    for _ in 0..iterations {
        let before = counters(store);
        let started = Instant::now();
        let mut work = op();
        let elapsed = started.elapsed();
        let after = counters(store);
        work.ledger_scans += after.0 - before.0;
        work.media_ledger_reads += after.1 - before.1;
        work.role_lookups += after.2 - before.2;
        values.push(elapsed.as_secs_f64() * 1_000.);
        match reference {
            Some(reference) => assert_eq!(
                work, reference,
                "{label}: deterministic work drifted between iterations; \
                 a gate on a drifting metric would be noise"
            ),
            None => reference = Some(work),
        }
        last = work;
    }
    values.sort_by(f64::total_cmp);
    Report {
        label: label.to_string(),
        iterations,
        p50_ms: percentile(&values, 50),
        p95_ms: percentile(&values, 95),
        max_ms: *values.last().unwrap(),
        work: last,
    }
}

fn counters(store: &Store) -> (usize, usize, usize) {
    (
        store.root_ledger_scans(),
        store.media_ledger_reads(),
        store.role_lookups(),
    )
}

fn percentile(sorted: &[f64], percent: usize) -> f64 {
    let index = (sorted.len() * percent).div_ceil(100).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn counter(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "n/a".into(),
    }
}

/// Prints the report table as Markdown, so it can be pasted into the audit doc unchanged.
pub fn print_table(header: &str, reports: &[Report]) {
    println!("\n### {header}\n");
    println!(
        "| operation | iterations | p50 ms | p95 ms | max ms | statements | full-scan steps | vm steps | ledger scans | media ledger reads | role lookups |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for report in reports {
        println!(
            "| {} | {} | {:.3} | {:.3} | {:.3} | {} | {} | {} | {} | {} | {} |",
            report.label,
            report.iterations,
            report.p50_ms,
            report.p95_ms,
            report.max_ms,
            counter(report.work.statements),
            counter(report.work.fullscan_steps),
            counter(report.work.vm_steps),
            report.work.ledger_scans,
            report.work.media_ledger_reads,
            report.work.role_lookups,
        );
    }
    println!();
}

/// One line of context that belongs with the table (SQLite version, fixture size).
pub fn print_context(source: &Store, scale_label: &str) {
    let library_bytes = std::fs::metadata(source.data_dir().join("library.db"))
        .map(|meta| meta.len())
        .unwrap_or_default();
    println!(
        "fixture: {scale_label}; sqlite {}; library.db {} B",
        rusqlite::version(),
        library_bytes
    );
}
