//! The private Store connections have one tracer owner: this scoped observer.
//! Ordinary queries never install a callback. Borrowed connections keep registration
//! identities stable; per-thread reference counts preserve nested observers. No SQL
//! or bound values are logged, and callbacks never execute another SQL statement.
use std::cell::RefCell;
use std::collections::HashMap;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::{Connection, StatementStatus};

use super::QueryWorkStats;

#[derive(Default)]
struct State {
    total: QueryWorkStats,
    connections: HashMap<usize, usize>,
    starts: HashMap<String, Vec<(u64, u64)>>,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

fn counter(value: i32) -> u64 {
    u64::from(value as u32)
}

fn observe(event: TraceEvent<'_>) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        match event {
            TraceEvent::Stmt(stmt, _) => {
                state
                    .starts
                    .entry(stmt.sql().into_owned())
                    .or_default()
                    .push((
                        counter(stmt.get_status(StatementStatus::VmStep)),
                        counter(stmt.get_status(StatementStatus::FullscanStep)),
                    ));
            }
            TraceEvent::Profile(stmt, _) => {
                let start = state.starts.get_mut(stmt.sql().as_ref()).and_then(Vec::pop);
                if let Some((vm, scan)) = start {
                    state.total.vm_steps +=
                        counter(stmt.get_status(StatementStatus::VmStep)).saturating_sub(vm);
                    state.total.fullscan_steps +=
                        counter(stmt.get_status(StatementStatus::FullscanStep))
                            .saturating_sub(scan);
                    state.total.statements += 1;
                }
            }
            TraceEvent::Row(_) => state.total.sql_rows += 1,
            _ => {}
        }
    });
}

pub(crate) struct QueryWorkScope<'a> {
    connections: Vec<&'a Connection>,
    before: QueryWorkStats,
}
impl<'a> QueryWorkScope<'a> {
    pub(crate) fn new(connections: &[&'a Connection]) -> Self {
        let before = STATE.with(|state| {
            let mut state = state.borrow_mut();
            for conn in connections {
                let users = state
                    .connections
                    .entry(*conn as *const Connection as usize)
                    .or_default();
                if *users == 0 {
                    conn.trace_v2(
                        TraceEventCodes::SQLITE_TRACE_STMT
                            | TraceEventCodes::SQLITE_TRACE_PROFILE
                            | TraceEventCodes::SQLITE_TRACE_ROW,
                        Some(observe),
                    );
                }
                *users += 1;
            }
            state.total
        });
        Self {
            connections: connections.to_vec(),
            before,
        }
    }

    pub(crate) fn stats(&self, rows_returned: u64) -> QueryWorkStats {
        STATE.with(|state| {
            let now = state.borrow().total;
            QueryWorkStats {
                vm_steps: now.vm_steps - self.before.vm_steps,
                fullscan_steps: now.fullscan_steps - self.before.fullscan_steps,
                statements: now.statements - self.before.statements,
                sql_rows: now.sql_rows - self.before.sql_rows,
                rows_returned,
            }
        })
    }
}
impl Drop for QueryWorkScope<'_> {
    fn drop(&mut self) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            for conn in &self.connections {
                let key = *conn as *const Connection as usize;
                if let Some(users) = state.connections.get_mut(&key) {
                    *users -= 1;
                    if *users == 0 {
                        conn.trace_v2(TraceEventCodes::empty(), None);
                        state.connections.remove(&key);
                    }
                }
            }
            if state.connections.is_empty() {
                state.starts.clear();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
