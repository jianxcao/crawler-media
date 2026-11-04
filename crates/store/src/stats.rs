//! Scoped SQL measurements attach only while the operation owns a connection lease.
use crate::library_query::items::stats::{QueryWorkScope, QueryWorkStats};
use crate::{Db, Store};
use rusqlite::Connection;
use std::{cell::RefCell, rc::Rc};

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatsScope {
    Library,
    App,
    Both,
    Subscribe,
    Cache,
    All,
}
struct Frame {
    scope: StatsScope,
    totals: Rc<RefCell<QueryWorkStats>>,
}
thread_local! { static FRAMES:RefCell<Vec<Frame>>=const { RefCell::new(Vec::new()) }; }

pub(crate) fn record_reader_checkout(database: &str) {
    let db = match database {
        "app" => Db::App,
        "library" => Db::Library,
        "subscribe" => Db::Subscribe,
        _ => Db::Cache,
    };
    FRAMES.with(|frames| {
        for frame in frames.borrow().iter() {
            if selected(frame.scope, db) {
                frame.totals.borrow_mut().reader_checkouts += 1;
            }
        }
    });
}

fn selected(scope: StatsScope, db: Db) -> bool {
    matches!(
        (scope, db),
        (StatsScope::All, _)
            | (StatsScope::Subscribe, Db::Subscribe)
            | (StatsScope::Cache, Db::Cache)
            | (StatsScope::Both, Db::App | Db::Library)
            | (StatsScope::App, Db::App)
            | (StatsScope::Library, Db::Library)
    )
}

pub(crate) fn record_writer(sample: &crate::writers::WriterSample) {
    FRAMES.with(|frames| {
        for frame in frames
            .borrow()
            .iter()
            .filter(|frame| selected(frame.scope, sample.database))
        {
            let mut total = frame.totals.borrow_mut();
            total.writer_wait_ns = total.writer_wait_ns.saturating_add(sample.writer_wait_ns);
            total.writer_hold_ns = total.writer_hold_ns.saturating_add(sample.writer_hold_ns);
            total.covered_owned_write_tx_count = total
                .covered_owned_write_tx_count
                .saturating_add(sample.covered_owned_write_tx_count);
            total.covered_owned_write_tx_ns = total
                .covered_owned_write_tx_ns
                .saturating_add(sample.covered_owned_write_tx_ns);
            total.covered_owned_write_tx_max_ns = total
                .covered_owned_write_tx_max_ns
                .max(sample.covered_owned_write_tx_max_ns);
        }
    });
}

pub(crate) struct LeaseStats<'a> {
    db: Db,
    scope: Option<QueryWorkScope<'a>>,
}
impl<'a> LeaseStats<'a> {
    pub(crate) fn for_database(database: &str, conn: &'a Connection) -> Self {
        match database {
            "app" => Self::new(Db::App, conn),
            "library" => Self::new(Db::Library, conn),
            "subscribe" => Self::new(Db::Subscribe, conn),
            _ => Self::new(Db::Cache, conn),
        }
    }
    pub(crate) fn new(db: Db, conn: &'a Connection) -> Self {
        let enabled = FRAMES.with(|frames| {
            frames
                .borrow()
                .iter()
                .any(|frame| selected(frame.scope, db))
        });
        Self {
            db,
            scope: enabled.then(|| QueryWorkScope::new(&[conn])),
        }
    }
}
impl Drop for LeaseStats<'_> {
    fn drop(&mut self) {
        let Some(scope) = &self.scope else {
            return;
        };
        let measured = scope.stats(0);
        FRAMES.with(|frames| {
            for frame in frames.borrow().iter() {
                if selected(frame.scope, self.db) {
                    add(&mut frame.totals.borrow_mut(), measured);
                }
            }
        });
    }
}
fn add(to: &mut QueryWorkStats, value: QueryWorkStats) {
    to.reader_checkouts += value.reader_checkouts;
    to.statements += value.statements;
    to.fullscan_steps += value.fullscan_steps;
    to.vm_steps += value.vm_steps;
    to.sort_operations += value.sort_operations;
    to.sql_rows += value.sql_rows;
    to.sql_result_value_bytes = to
        .sql_result_value_bytes
        .saturating_add(value.sql_result_value_bytes);
    to.sql_duration_ns += value.sql_duration_ns;
    to.rows_returned += value.rows_returned;
    to.hydrated_rows += value.hydrated_rows;
}
impl Store {
    #[doc(hidden)]
    pub fn with_stats<T>(
        &self,
        scope: StatsScope,
        work: impl FnOnce() -> T,
    ) -> (T, QueryWorkStats) {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                FRAMES.with(|frames| {
                    frames.borrow_mut().pop();
                });
            }
        }
        let totals = Rc::new(RefCell::new(QueryWorkStats::default()));
        FRAMES.with(|frames| {
            frames.borrow_mut().push(Frame {
                scope,
                totals: Rc::clone(&totals),
            })
        });
        let restore = Restore;
        let value = work();
        drop(restore);
        let stats = *totals.borrow();
        (value, stats)
    }
}
