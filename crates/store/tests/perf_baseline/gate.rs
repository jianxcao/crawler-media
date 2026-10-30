//! The gate: pinned deterministic budgets, the two entry points, and the negative controls
//! that prove the gate actually bites.
//!
//! Budgets are per-operation **upper bounds on deterministic counters**, never on time:
//! - `statements`: SQL statements the operation executed.
//! - `fullscan_steps`, `vm_steps`, sort operations and decoded SQL rows: an indexed
//!   prefix scan can do excessive work without reporting any full-table scan steps.
//! - `ledger_scans` / `media_ledger_reads` / `role_lookups`: the store's own diagnostics, so
//!   an operation that quietly starts re-reading the whole ledger or the roster fails even
//!   where no statement counter exists.
//!
//! An operation may get *cheaper* without touching this file. Getting more expensive means
//! the change has to be justified and the budget raised in the same commit, which is the
//! whole point: the difference between "I think it got faster" and a number.
//!
//! Statement budgets are scale independent (same queries, same page sizes). The full-scan
//! budgets for the facet dimensions are not: facet counting currently visits the per-card
//! term and watch rows, so those carry a calibrated per-file allowance
//! (`base + files * per_1k / 1000`) fitted at both measured scales with ~20% headroom. A
//! second scan of the same rows still fails for nine of the ten dimensions; `hdr` reads only
//! the member marginal, so it is pinned at zero.

use super::fixture::{Fixture, Scale};
use super::measure::{Report, print_context, print_table};
use super::ops;

/// One pinned budget.
///
/// `statements` and `fullscan` use the same shape, `base + files * per_1k / 1000`:
/// a scale-independent operation pins `per_1k = 0`, and an operation whose work follows the
/// number of published files (scan publication) pins a calibrated slope. `statements: None`
/// means "the store exposes no counter for this operation yet" (see §7 of the audit doc).
struct Limit {
    label: String,
    statements: Option<(u64, u64)>,
    /// `fullscan_steps <= base + files * per_1k / 1000`.
    fullscan: (u64, u64),
    /// `vm_steps <= base + files * per_1k / 1000`, when a dimension's row work is pinned.
    ///
    /// VM steps are deterministic row work, not elapsed time. They catch expensive
    /// indexed scans even when SQLite reports zero full-scan steps.
    vm: Option<(u64, u64)>,
    sorts: Option<u64>,
    sql_rows: Option<u64>,
    allowed_ledger_scans: usize,
    allowed_role_lookups: usize,
}

/// A budget whose counters do not grow with the fixture: `statements <= n`, no scan work.
fn limit(label: &str, statements: Option<u64>, fullscan: (u64, u64)) -> Limit {
    Limit {
        label: label.to_string(),
        statements: statements.map(|n| (n, 0)),
        fullscan,
        vm: None,
        sorts: None,
        sql_rows: None,
        allowed_ledger_scans: 0,
        allowed_role_lookups: 0,
    }
}

/// A budget whose counters grow with the fixture: `base + files * per_1k / 1000`.
fn scaled_limit(label: &str, statements: Option<(u64, u64)>, fullscan: (u64, u64)) -> Limit {
    Limit {
        label: label.to_string(),
        statements,
        fullscan,
        vm: None,
        sorts: None,
        sql_rows: None,
        allowed_ledger_scans: 0,
        allowed_role_lookups: 0,
    }
}

/// A facet dimension: statement count, full-scan allowance and a pinned `vm_steps` band.
fn facet_limit(label: &str, statements: u64, fullscan: (u64, u64), vm: (u64, u64)) -> Limit {
    Limit {
        label: label.to_string(),
        statements: Some((statements, 0)),
        fullscan,
        vm: Some(vm),
        sorts: None,
        sql_rows: None,
        allowed_ledger_scans: 0,
        allowed_role_lookups: 0,
    }
}

fn statements_budget(limit: &Limit, files: usize) -> Option<u64> {
    limit
        .statements
        .map(|(base, per_1k)| base + (files as u64) * per_1k / 1_000)
}

/// No full-scan work at all: the operation must stay on its indexes.
const ZERO_SCAN: (u64, u64) = (0, 0);

/// An operation that authorizes one caller: exactly one role lookup, plus the operation's
/// own statement and scan budgets.
fn role_limit(label: &str, statements: Option<u64>, fullscan: (u64, u64)) -> Limit {
    Limit {
        allowed_role_lookups: 1,
        ..limit(label, statements, fullscan)
    }
}

fn limits(store: &store::Store) -> Vec<Limit> {
    let mut limits = vec![
        Limit {
            // The fixed 60-card page measures 1,938 VM steps at both 424 and
            // 61,000 files. Allow 24% headroom, but no sort or off-page hydration.
            vm: Some((2_400, 0)),
            sorts: Some(0),
            sql_rows: Some(70),
            ..limit(ops::BROWSE_PAGE_1, Some(6), ZERO_SCAN)
        },
        limit(ops::BROWSE_DEEP_OFFSET, Some(6), ZERO_SCAN),
        limit(ops::BROWSE_DEEP_CURSOR, Some(5), ZERO_SCAN),
        // Task 11 landed: the lease no longer validates the whole layout per request, so the
        // budget is tightened from 277 statements / 25,188 scan steps to just above the
        // measured 17 / 4. An upper bound that still allowed the old behaviour could not
        // detect a regression back to it.
        limit(ops::BROWSE_READ_LEASE, Some(20), (8, 0)),
        // Task 21: these rows used to be `None` — the store exposed no counter, so the gate
        // could only time them. They are measured through `Store::with_stats` around the
        // production entry point, and every one below is identical at both scales
        // (424 and 61,000 files), so the statement budgets are flat.
        //
        // detail: 11 statements for the row + file page. 12 leaves room for the fixture's
        // second version only — a per-file query (2 files movie / 9 files tv) would fail.
        limit(ops::ITEM_DETAIL, Some(12), ZERO_SCAN),
        limit(ops::ITEM_DETAIL_TV, Some(12), ZERO_SCAN),
        // Playback resolves the caller's role once per selection; that is the only
        // allowance (a second lookup per request would fail).
        role_limit(ops::PLAYBACK_ROSTER, Some(3), (8, 0)),
        role_limit(ops::PLAYBACK_SOURCES, Some(4), (8, 0)),
        // Publication is the one scale-dependent row: 8.05 statements and 4 full-scan steps
        // per published file, calibrated through both measured scales (+12 % headroom).
        // The slope is fitted against `files` because that is the only scale the checker
        // knows; changing `publish_slots` independently of the file count means recalibrating.
        scaled_limit(ops::PUBLISH_INSERT, Some((1_750, 120)), (900, 60)),
        scaled_limit(ops::PUBLISH_REPUBLISH, Some((1_750, 120)), (900, 60)),
        // The heartbeat: one event per client tick, 23 statements (write tx, event row,
        // per-user summary, counters). 25 catches a second reduction or an extra round trip.
        role_limit(ops::PLAYBACK_PROGRESS_WRITE, Some(25), (8, 0)),
        limit(ops::PLAYBACK_UNSCOPED, Some(2), ZERO_SCAN),
        limit(ops::INTEGRITY_STATUS, Some(4), ZERO_SCAN),
    ];
    // `(statements, fullscan base, fullscan per 1k files, vm base, vm per 1k files)`, every
    // pair fitted from the 424-file and 61,000-file measurements with ~15% headroom.
    for (dimension, statements, fullscan, vm) in [
        ("genres", 6, (520, 700), (13_592, 16_732)),
        ("countries", 6, (986, 781), (16_204, 21_058)),
        ("decades", 6, (706, 546), (9_286, 12_294)),
        ("watch", 5, (2_206, 1_907), (39_853, 57_915)),
        ("ratings", 5, (1_358, 1_189), (25_371, 37_318)),
        ("runtimes", 6, (1_017, 781), (18_761, 21_197)),
        ("languages", 6, (986, 781), (16_207, 21_058)),
        ("resolutions", 6, (1_217, 953), (19_804, 25_460)),
        ("hdr", 6, ZERO_SCAN, (2_607, 2_852)),
        ("stock", 6, (756, 654), (9_961, 12_166)),
    ] {
        limits.push(facet_limit(
            &ops::facet_label(dimension),
            statements,
            fullscan,
            vm,
        ));
    }
    add_reader_overhead(store, &mut limits);
    limits
}

/// Old budgets observed business SQL on a writer connection; full read leases now
/// also include layout checks and snapshot boundaries. Measure their fixed cost
/// separately, retain it in reports, and keep the old query-work allowances.
fn add_reader_overhead(store: &store::Store, limits: &mut [Limit]) {
    let (_, empty) = store.with_stats(store::stats::StatsScope::Both, || {
        store.with_read(store::Db::Library, |_| Ok(())).unwrap();
    });
    for limit in limits {
        let (leases, previous_snapshot) = match limit.label.as_str() {
            ops::BROWSE_PAGE_1 | ops::BROWSE_DEEP_OFFSET | ops::BROWSE_DEEP_CURSOR => (1, 2),
            ops::ITEM_DETAIL | ops::ITEM_DETAIL_TV | ops::PLAYBACK_UNSCOPED => (1, 0),
            ops::PLAYBACK_ROSTER => (2, 0),
            ops::PLAYBACK_SOURCES => (3, 0),
            // Library stats deliberately exclude the App role lookup.
            ops::PLAYBACK_PROGRESS_WRITE => (1, 0),
            ops::INTEGRITY_STATUS => (3, 0),
            label if label.starts_with("facets:") => (1, 2),
            // The explicit read-lease budget already included the entire lease.
            _ => (0, 0),
        };
        if let Some((base, _)) = &mut limit.statements {
            *base += leases * empty.statements - previous_snapshot;
        }
        if let Some((base, _)) = &mut limit.vm {
            *base += leases * empty.vm_steps;
        }
        if let Some(rows) = &mut limit.sql_rows {
            *rows += leases * empty.sql_rows;
        }
    }
}

fn fullscan_budget(limit: &Limit, files: usize) -> u64 {
    scaled(limit.fullscan, files)
}

fn scaled((base, per_1k): (u64, u64), files: usize) -> u64 {
    base + (files as u64) * per_1k / 1_000
}

/// Compares one report with its budget; returns every violated metric.
fn check(report: &Report, limit: &Limit, files: usize) -> Vec<String> {
    let mut violations = Vec::new();
    if let (Some(measured), Some(budget)) =
        (report.work.statements, statements_budget(limit, files))
        && measured > budget
    {
        violations.push(format!(
            "{}: {} SQL statements, budget {}",
            limit.label, measured, budget
        ));
    }
    if let Some(measured) = report.work.fullscan_steps {
        let budget = fullscan_budget(limit, files);
        if measured > budget {
            violations.push(format!(
                "{}: {} full-scan steps, budget {}",
                limit.label, measured, budget
            ));
        }
    }
    if let (Some(measured), Some(budget)) = (report.work.vm_steps, limit.vm) {
        let budget = scaled(budget, files);
        if measured > budget {
            violations.push(format!(
                "{}: {} vm steps, budget {} (row work grew without a new statement)",
                limit.label, measured, budget
            ));
        }
    }
    if report.work.ledger_scans > limit.allowed_ledger_scans {
        violations.push(format!(
            "{}: {} full ledger scans, budget {}",
            limit.label, report.work.ledger_scans, limit.allowed_ledger_scans
        ));
    }
    for (name, measured, budget) in [
        ("sort operations", report.work.sort_operations, limit.sorts),
        ("SQL result rows", report.work.sql_rows, limit.sql_rows),
    ] {
        if let (Some(measured), Some(budget)) = (measured, budget)
            && measured > budget
        {
            violations.push(format!(
                "{}: {measured} {name}, budget {budget}",
                limit.label
            ));
        }
    }
    if report.work.media_ledger_reads > 0 {
        violations.push(format!(
            "{}: {} per-media ledger reads, budget 0",
            limit.label, report.work.media_ledger_reads
        ));
    }
    if report.work.role_lookups > limit.allowed_role_lookups {
        violations.push(format!(
            "{}: {} role lookups, budget {}",
            limit.label, report.work.role_lookups, limit.allowed_role_lookups
        ));
    }
    violations
}

/// The control deliberately does something expensive; it is measured to prove the counters
/// are live, not to be held to a budget.
fn is_control(label: &str) -> bool {
    label.starts_with("control:")
}

/// Every operation must be covered by a budget, and every budget must be respected.
fn assert_gate(reports: &[Report], files: usize, store: &store::Store) {
    let limits = limits(store);
    let mut violations = Vec::new();
    for report in reports {
        if is_control(&report.label) {
            continue;
        }
        match limits.iter().find(|limit| limit.label == report.label) {
            Some(limit) => violations.extend(check(report, limit, files)),
            None => violations.push(format!(
                "{}: no budget pinned for this operation",
                report.label
            )),
        }
    }
    for limit in &limits {
        if !reports.iter().any(|report| report.label == limit.label) {
            violations.push(format!("{}: budget pinned but not measured", limit.label));
        }
    }
    assert!(
        violations.is_empty(),
        "performance gate failed:\n  {}",
        violations.join("\n  ")
    );
}

fn run_gate(scale: Scale) {
    let fixture = Fixture::build(scale);
    print_context(&fixture.store, &scale.label());
    let reports = ops::all(&fixture);
    print_table(&scale.label(), &reports);
    assert_gate(&reports, scale.files(), &fixture.store);
}

/// The default-target variant: same fixture shape and the same budgets, 1/144 of the rows.
#[test]
fn small_scale_gate() {
    run_gate(Scale::SMALL);
}

/// The gate proper. Two minutes of seeding, so it lives behind `--ignored`/`--release`.
#[test]
#[ignore = "realistic 20k Media / 61k file fixture; run with --release --ignored"]
fn realistic_scale_gate() {
    run_gate(Scale::REALISTIC);
}

// ---------------------------------------------------------------------------
// Negative controls: the ways this gate could quietly stop working.
//
// Each one measures a real store operation, breaks only a throwaway fixture database
// (nothing in `crates/store/src` is touched), and asserts that the *same* `check` the gate
// uses reports a violation. They run in the default target, so a gate that stopped biting
// fails immediately.
// ---------------------------------------------------------------------------

/// The full-scan counter must be live: `Store::list_file_coverage` walks every file row, so
/// a zero budget has to reject it. This is the "query without the index" case.
#[test]
fn gate_rejects_a_full_table_scan() {
    let fixture = Fixture::build(Scale::TINY);
    let report = ops::ledger_scan_report(&fixture, ops::CONTROL_FULL_SCAN);
    assert!(
        report.work.ledger_scans > 0,
        "the coverage read reported no scan work: {:?}",
        report.work
    );
    let violations = check(
        &report,
        &limit(ops::CONTROL_FULL_SCAN, None, ZERO_SCAN),
        fixture.scale.files(),
    );
    assert!(
        !violations.is_empty(),
        "the gate accepted a full table scan: {:?}",
        report.work
    );
    println!("negative control (full table scan) rejected as designed: {violations:?}");
}

/// The D1 regression shape: `delivery.rs` used to call `list_file_coverage` inside the Store
/// lock on every request. The detail read itself is unchanged, but the ledger-scan counter
/// catches the extra scan.
#[test]
fn gate_rejects_a_ledger_scan_inside_a_hot_read() {
    let fixture = Fixture::build(Scale::TINY);
    let label = "detail + full ledger scan";
    let report = ops::detail_with_ledger_scan_report(&fixture, label);
    assert!(
        report.work.ledger_scans > 0,
        "the simulated regression did not scan: {:?}",
        report.work
    );
    let violations = check(
        &report,
        &limit(label, None, ZERO_SCAN),
        fixture.scale.files(),
    );
    assert!(
        !violations.is_empty(),
        "the gate accepted a hot read that scans the whole ledger: {:?}",
        report.work
    );
    println!("negative control (ledger scan in a hot read) rejected as designed: {violations:?}");
}

/// An N+1 regression: running the page query twice keeps the full-scan count at zero but
/// doubles the statement count, which the budget must reject.
#[test]
fn gate_rejects_a_duplicated_query() {
    let fixture = Fixture::build(Scale::TINY);
    let label = "browse page 1 (query run twice)";
    let once = ops::browse_report(&fixture, label, 20);
    let budget = limit(label, once.work.statements, ZERO_SCAN);
    let doubled = ops::browse_report_twice(&fixture, label, 20);
    let violations = check(&doubled, &budget, fixture.scale.files());
    assert!(
        !violations.is_empty(),
        "the gate accepted a duplicated query: {:?} against budget {:?}",
        doubled.work,
        budget.statements
    );
    println!("negative control (duplicated query) rejected as designed: {violations:?}");
}

/// A supported query can still do excessive row work without changing schema.
/// Keep proving the VM budget rejects it independently of layout validation.
#[test]
fn gate_rejects_excessive_indexed_page_work() {
    let fixture = Fixture::build(Scale::TINY);
    let budget = limits(&fixture.store)
        .into_iter()
        .find(|limit| limit.label == ops::BROWSE_PAGE_1)
        .unwrap();
    let report = ops::browse_report(&fixture, ops::BROWSE_PAGE_1, fixture.scale.movies);
    let violations = check(&report, &budget, fixture.scale.files());
    assert!(
        violations
            .iter()
            .any(|message| message.contains("vm steps")),
        "the VM budget accepted excessive indexed row work: {:?}, {violations:?}",
        report.work
    );
}

/// Unified readers validate schema changes before serving another lease. A
/// missing required index must reject the layout, before a slower query can run.
#[test]
fn a_lost_index_fails_loudly_instead_of_scanning() {
    let fixture = Fixture::build(Scale::TINY);
    assert_lost_index_rejected(fixture);
}

#[test]
fn lost_index_is_rejected_with_fresh_and_analyzed_statistics() {
    for analyze in [false, true] {
        let fixture = Fixture::fresh(Scale::TINY);
        if analyze {
            let db =
                rusqlite::Connection::open(fixture.store.data_dir().join("library.db")).unwrap();
            db.execute_batch("ANALYZE").unwrap();
        }
        assert_lost_index_rejected(fixture);
    }
}

fn assert_lost_index_rejected(fixture: Fixture) {
    let budget = limits(&fixture.store)
        .into_iter()
        .find(|limit| limit.label == ops::BROWSE_PAGE_1)
        .unwrap();
    let healthy = ops::browse_report(&fixture, ops::BROWSE_PAGE_1, 60);
    assert!(
        check(&healthy, &budget, fixture.scale.files()).is_empty(),
        "the healthy run must fit its budget: {:?}",
        healthy.work
    );

    drop_index(&fixture, "items_title");
    let error = fixture
        .store
        .library_item_page(&ops::page_query(&fixture))
        .unwrap_err();
    assert!(
        error.is_layout_refusal(),
        "a missing required index must reject the changed layout: {error}"
    );
}

/// Drops one named index from the fixture's own `library.db`, exactly as a missing
/// `CREATE INDEX` would leave it. The next lease must revalidate the schema cookie.
fn drop_index(fixture: &Fixture, name: &str) {
    let db = rusqlite::Connection::open(fixture.store.data_dir().join("library.db")).unwrap();
    db.execute_batch(&format!("DROP INDEX \"{name}\"")).unwrap();
}

/// The measurement seam itself must be live.
///
/// Every budget added in Task 21 is a *flat* number taken from `Store::with_stats`. If that
/// seam ever reported zeros — a trace that stopped registering, a scope that measured the
/// wrong connection — every one of those budgets would pass vacuously and the gate would be
/// decorative. So it is pinned in both directions: a known full scan must report scan work,
/// and a keyed page must report none.
#[test]
fn with_stats_reports_the_work_the_database_really_did() {
    use store::stats::StatsScope;

    let fixture = Fixture::build(Scale::TINY);
    let (coverage, scanning) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.list_file_coverage().unwrap()
    });
    assert!(!coverage.is_empty(), "the coverage read returned nothing");
    assert!(
        scanning.statements >= 1 && scanning.fullscan_steps > 0,
        "the seam did not see the full ledger scan: {scanning:?}"
    );

    let query = ops::page_query(&fixture);
    let (_, keyed) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.library_item_page(&query).unwrap()
    });
    assert!(
        keyed.statements >= 1,
        "the seam counted no statements for a keyed page: {keyed:?}"
    );
    assert_eq!(
        keyed.fullscan_steps, 0,
        "a keyed page must not scan: {keyed:?}"
    );

    // Nested scopes must not double-count the inner operation in the outer one.
    let (_, outer) = fixture.store.with_stats(StatsScope::Library, || {
        let (_, inner) = fixture.store.with_stats(StatsScope::Library, || {
            fixture.store.list_file_coverage().unwrap()
        });
        assert!(
            inner.fullscan_steps > 0,
            "inner scope saw no scan: {inner:?}"
        );
    });
    assert!(
        outer.fullscan_steps >= scanning.fullscan_steps,
        "the outer scope must include the inner read: outer {outer:?} vs direct {scanning:?}"
    );
    println!("seam control: full scan {scanning:?}, keyed page {keyed:?}, nested {outer:?}");
}
