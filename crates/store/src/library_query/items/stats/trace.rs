//! The private Store connections have one tracer owner: this scoped observer.
//! Ordinary queries never install a callback. Borrowed connections keep registration
//! identities stable; per-thread reference counts preserve nested observers. No SQL
//! or bound values are logged, and callbacks never execute another SQL statement.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use rusqlite::{Connection, ffi};

use super::QueryWorkStats;

#[derive(Default)]
struct State {
    totals: HashMap<usize, QueryWorkStats>,
    connections: HashMap<usize, usize>,
    starts: HashMap<usize, (u64, u64, u64)>,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

fn counter(value: i32) -> u64 {
    u64::from(value as u32)
}

fn status(stmt: *mut ffi::sqlite3_stmt) -> (u64, u64, u64) {
    // SQLite invokes the callback with a live sqlite3_stmt; reset=0 reads its
    // cumulative counters without changing the prepared statement's state.
    unsafe {
        (
            counter(ffi::sqlite3_stmt_status(
                stmt,
                ffi::SQLITE_STMTSTATUS_VM_STEP,
                0,
            )),
            counter(ffi::sqlite3_stmt_status(
                stmt,
                ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP,
                0,
            )),
            counter(ffi::sqlite3_stmt_status(
                stmt,
                ffi::SQLITE_STMTSTATUS_SORT,
                0,
            )),
        )
    }
}

fn observe(connection: usize, event: c_uint, p: *mut c_void, x: *mut c_void) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let stmt = p.cast::<ffi::sqlite3_stmt>();
        match event {
            ffi::SQLITE_TRACE_STMT => {
                state.starts.insert(stmt as usize, status(stmt));
            }
            ffi::SQLITE_TRACE_PROFILE => {
                let start = state.starts.remove(&(stmt as usize));
                if let Some((vm, scan, sort)) = start {
                    let (after_vm, after_scan, after_sort) = status(stmt);
                    let total = state.totals.entry(connection).or_default();
                    total.vm_steps = total.vm_steps.saturating_add(after_vm.saturating_sub(vm));
                    total.fullscan_steps = total
                        .fullscan_steps
                        .saturating_add(after_scan.saturating_sub(scan));
                    total.sort_operations = total
                        .sort_operations
                        .saturating_add(after_sort.saturating_sub(sort));
                    total.statements = total.statements.saturating_add(1);
                    // PROFILE passes a live pointer to SQLite's nanosecond duration.
                    let duration_ns = unsafe { *x.cast::<i64>() };
                    total.sql_duration_ns = total
                        .sql_duration_ns
                        .saturating_add(u64::try_from(duration_ns).unwrap_or_default());
                }
            }
            ffi::SQLITE_TRACE_ROW => {
                let total = state.totals.entry(connection).or_default();
                total.sql_rows = total.sql_rows.saturating_add(1);
            }
            _ => {}
        }
    });
}

unsafe extern "C" fn trace_callback(
    event: c_uint,
    context: *mut c_void,
    statement: *mut c_void,
    extra: *mut c_void,
) -> c_int {
    // SQLite owns both callback pointers for this invocation. Catch panics so
    // Rust never unwinds across its C callback boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        observe(context as usize, event, statement, extra)
    }));
    ffi::SQLITE_OK
}

fn register(conn: &Connection, enabled: bool) {
    let mask = if enabled {
        ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_ROW
    } else {
        0
    };
    // The borrowed Connection keeps this handle alive throughout registration;
    // no pointer is retained. The callback keys work by SQLite's actual stmt ptr.
    unsafe {
        ffi::sqlite3_trace_v2(
            conn.handle(),
            mask,
            enabled.then_some(trace_callback),
            conn.handle().cast::<c_void>(),
        );
    }
}

pub(crate) struct QueryWorkScope<'a> {
    connections: Vec<&'a Connection>,
    before: HashMap<usize, QueryWorkStats>,
}
impl<'a> QueryWorkScope<'a> {
    pub(crate) fn new(connections: &[&'a Connection]) -> Self {
        let before = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let mut before = HashMap::new();
            for conn in connections {
                // The connection remains borrowed for this scope. SQLite owns this handle.
                let key = unsafe { conn.handle() } as usize;
                before.insert(key, state.totals.get(&key).copied().unwrap_or_default());
                let users = state.connections.entry(key).or_default();
                if *users == 0 {
                    register(conn, true);
                }
                *users += 1;
            }
            before
        });
        Self {
            connections: connections.to_vec(),
            before,
        }
    }

    pub(crate) fn stats(&self, rows_returned: u64) -> QueryWorkStats {
        STATE.with(|state| {
            let state = state.borrow();
            let mut result = QueryWorkStats {
                rows_returned,
                ..Default::default()
            };
            for (key, before) in &self.before {
                let now = state.totals.get(key).copied().unwrap_or_default();
                result.vm_steps += now.vm_steps.saturating_sub(before.vm_steps);
                result.fullscan_steps += now.fullscan_steps.saturating_sub(before.fullscan_steps);
                result.sort_operations +=
                    now.sort_operations.saturating_sub(before.sort_operations);
                result.statements += now.statements.saturating_sub(before.statements);
                result.sql_duration_ns +=
                    now.sql_duration_ns.saturating_sub(before.sql_duration_ns);
                result.sql_rows += now.sql_rows.saturating_sub(before.sql_rows);
            }
            result
        })
    }
}
impl Drop for QueryWorkScope<'_> {
    fn drop(&mut self) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            for conn in &self.connections {
                // Connection is still borrowed and live during unregister.
                let key = unsafe { conn.handle() } as usize;
                if let Some(users) = state.connections.get_mut(&key) {
                    *users -= 1;
                    if *users == 0 {
                        register(conn, false);
                        state.connections.remove(&key);
                        state.totals.remove(&key);
                    }
                }
            }
            if state.connections.is_empty() {
                state.starts.clear();
            }
        });
    }
}

pub(crate) struct QueryWorkCapture<'a> {
    scope: QueryWorkScope<'a>,
    output: &'a mut QueryWorkStats,
}
impl<'a> QueryWorkCapture<'a> {
    pub(crate) fn new(conn: &'a Connection, output: &'a mut QueryWorkStats) -> Self {
        Self {
            scope: QueryWorkScope::new(&[conn]),
            output,
        }
    }
}
impl Drop for QueryWorkCapture<'_> {
    fn drop(&mut self) {
        *self.output = self.scope.stats(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;

    #[test]
    fn nested_observers_restore_the_outer_registration() {
        let db = Connection::open_in_memory().unwrap();
        let outer = QueryWorkScope::new(&[&db]);
        db.query_row("SELECT 1", [], |_| Ok(())).unwrap();
        {
            let inner = QueryWorkScope::new(&[&db]);
            db.query_row("SELECT 2", [], |_| Ok(())).unwrap();
            assert_eq!(inner.stats(0).statements, 1);
        }
        db.query_row("SELECT 3", [], |_| Ok(())).unwrap();
        assert_eq!(outer.stats(0).statements, 3);
        drop(outer);
        db.query_row("SELECT 4", [], |_| Ok(())).unwrap();
        let next = QueryWorkScope::new(&[&db]);
        db.query_row("SELECT 5", [], |_| Ok(())).unwrap();
        assert_eq!(next.stats(0).statements, 1);
    }

    #[test]
    fn failed_statement_and_nested_connection_do_not_leak_work() {
        let db = Connection::open_in_memory().unwrap();
        let other = Connection::open_in_memory().unwrap();
        let scope = QueryWorkScope::new(&[&db, &other]);
        db.execute_batch("CREATE TABLE facts(value INTEGER)")
            .unwrap();
        assert!(
            db.execute("INSERT INTO missing(value) VALUES(1)", [])
                .is_err()
        );
        let failed = scope.stats(0);
        other.query_row("SELECT 1", [], |_| Ok(())).unwrap();
        let after = scope.stats(0);
        assert!(after.statements > failed.statements);
        assert!(after.vm_steps > failed.vm_steps);
        assert!(after.sql_rows > failed.sql_rows);
        assert!(after.sql_duration_ns >= failed.sql_duration_ns);
        drop(scope);
        let fresh = QueryWorkScope::new(&[&db, &other]);
        other.query_row("SELECT 2", [], |_| Ok(())).unwrap();
        assert_eq!(fresh.stats(0).statements, 1);
    }

    #[test]
    fn interleaved_equal_sql_on_two_connections_keeps_each_counter_baseline() {
        let first = Connection::open_in_memory().unwrap();
        let second = Connection::open_in_memory().unwrap();
        for db in [&first, &second] {
            db.execute_batch(
                "CREATE TABLE facts(value INTEGER); WITH RECURSIVE n(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM n WHERE value<100) INSERT INTO facts SELECT value FROM n;",
            )
            .unwrap();
        }
        let sql = "SELECT value FROM facts ORDER BY value DESC";
        let mut reused = first.prepare(sql).unwrap();
        let mut fresh = second.prepare(sql).unwrap();
        for _ in 0..10 {
            let mut rows = reused.query([]).unwrap();
            while rows.next().unwrap().is_some() {}
        }
        let statuses = |stmt: &rusqlite::Statement<'_>| {
            (
                counter(stmt.get_status(StatementStatus::VmStep)),
                counter(stmt.get_status(StatementStatus::FullscanStep)),
                counter(stmt.get_status(StatementStatus::Sort)),
            )
        };
        let before_a = statuses(&reused);
        let before_b = statuses(&fresh);
        let scope = QueryWorkScope::new(&[&first, &second]);
        let mut rows_a = reused.query([]).unwrap();
        assert!(rows_a.next().unwrap().is_some());
        let mut rows_b = fresh.query([]).unwrap();
        assert!(rows_b.next().unwrap().is_some());
        while rows_a.next().unwrap().is_some() {}
        while rows_b.next().unwrap().is_some() {}
        drop(rows_a);
        drop(rows_b);
        let after_a = statuses(&reused);
        let after_b = statuses(&fresh);
        let measured = scope.stats(200);
        assert_eq!(measured.statements, 2);
        assert_eq!(
            measured.vm_steps,
            after_a.0 - before_a.0 + after_b.0 - before_b.0,
            "equal SQL on distinct prepared statements must keep separate VM baselines"
        );
        assert_eq!(
            measured.fullscan_steps,
            after_a.1 - before_a.1 + after_b.1 - before_b.1
        );
        assert_eq!(
            measured.sort_operations,
            after_a.2 - before_a.2 + after_b.2 - before_b.2
        );
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;

    #[test]
    fn distinct_scopes_on_interleaved_connections_attribute_only_their_sql() {
        let first = Connection::open_in_memory().unwrap();
        let second = Connection::open_in_memory().unwrap();
        let first_scope = QueryWorkScope::new(&[&first]);
        let second_scope = QueryWorkScope::new(&[&second]);
        first.query_row("SELECT 1", [], |_| Ok(())).unwrap();
        second.query_row("SELECT 2", [], |_| Ok(())).unwrap();
        second.query_row("SELECT 3", [], |_| Ok(())).unwrap();
        assert_eq!(first_scope.stats(0).statements, 1);
        assert_eq!(second_scope.stats(0).statements, 2);
    }

    #[test]
    fn concurrent_connections_match_their_isolated_work_after_error_and_unwind() {
        fn sample(
            rows: usize,
            barrier: Option<std::sync::Arc<std::sync::Barrier>>,
        ) -> QueryWorkStats {
            let conn = Connection::open_in_memory().unwrap();
            let mut stats = QueryWorkStats::default();
            let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _capture = QueryWorkCapture::new(&conn, &mut stats);
                if let Some(barrier) = barrier {
                    barrier.wait();
                }
                conn.query_row("WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v < ?1) SELECT sum(v) FROM n", [rows], |_| Ok(())).unwrap();
                assert!(conn.execute_batch("SELECT missing FROM missing").is_err());
                panic!("diagnostic observer unwind");
            }));
            assert!(panic.is_err());
            assert_eq!(stats.statements, 1);
            assert!(stats.vm_steps > 0);
            conn.query_row("SELECT 9", [], |_| Ok(())).unwrap();
            let next = QueryWorkScope::new(&[&conn]);
            conn.query_row("SELECT 1", [], |_| Ok(())).unwrap();
            assert_eq!(
                next.stats(0).statements,
                1,
                "unwind must unregister the previous scope"
            );
            stats
        }
        let baseline_small = sample(10, None);
        let baseline_large = sample(10000, None);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let peer = barrier.clone();
        let small = std::thread::spawn(move || sample(10, Some(peer)));
        let large = std::thread::spawn(move || sample(10000, Some(barrier)));
        assert_eq!(small.join().unwrap().vm_steps, baseline_small.vm_steps);
        assert_eq!(large.join().unwrap().vm_steps, baseline_large.vm_steps);
    }
}
