use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::test_support::gate::DEFAULT_GATE_TIMEOUT;
use crate::{LibraryReadError, QueryBudget, Store};

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + Duration::from_secs(3),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

/// Budget for the test that pauses a read while another thread publishes: the
/// window must outlast a slow scheduler on CI, not just the fixture work, or a
/// coordination bug shows up as a bogus `Deadline` error instead of a failure.
fn coordination_budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + DEFAULT_GATE_TIMEOUT,
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

fn busy_timeout(conn: &rusqlite::Connection) -> u64 {
    conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .unwrap()
}

#[test]
fn short_policy_restores_after_error_cancel_and_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = budget();
    let cancelled = Arc::clone(&first.cancelled);
    let result = store.with_query_budget(&first, |s| {
        assert!(busy_timeout(&s.app) <= 3000);
        assert!(busy_timeout(&s.library) <= 3000);
        cancelled.store(true, Ordering::Release);
        Err::<(), _>(LibraryReadError::Unauthorized)
    });
    assert!(matches!(result, Err(LibraryReadError::Cancelled)));
    assert_eq!(busy_timeout(&store.app), 5000);
    assert_eq!(busy_timeout(&store.library), 5000);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), LibraryReadError> =
            store.with_query_budget(&budget(), |_s| panic!("short fault"));
    }));
    assert_eq!(busy_timeout(&store.app), 5000);
    assert_eq!(busy_timeout(&store.library), 5000);
    let count = store
        .with_query_budget(&budget(), |s| {
            Ok(s.library
                .query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get::<_, i64>(0))
                .map_err(crate::StoreError::from)?)
        })
        .unwrap();
    assert!(count >= 0);
}

#[test]
fn sql_error_does_not_leave_short_policy_installed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let result: Result<(), LibraryReadError> = store.with_query_budget(&budget(), |s| {
        s.library
            .execute_batch("SELECT * FROM no_such_table")
            .map_err(crate::StoreError::from)?;
        Ok(())
    });
    assert!(matches!(result, Err(LibraryReadError::Store(_))));
    assert_eq!(busy_timeout(&store.app), 5000);
    assert_eq!(busy_timeout(&store.library), 5000);
}

#[path = "../../../tests/query_work/support.rs"]
#[allow(clippy::duplicate_mod)]
mod support;

#[test]
fn count_and_page_keep_one_library_snapshot_during_a_concurrent_publish() {
    use crate::library_query::items::{
        hydration::library_item_facts_on, page::library_item_page_on,
    };
    use crate::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    use std::num::NonZeroUsize;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = support::seed(&store, domain::MediaKind::Movie, 2);
    let query = LibraryQuery {
        library_id: library,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: true,
    };
    let before = store.library_item_page(&query).unwrap();
    let pool = store
        .open_library_read_pool(NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let (start, started) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel();
    let path = dir.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        started
            .recv_timeout(DEFAULT_GATE_TIMEOUT)
            .expect("the read never signalled that it had started");
        let store = Store::open(&path).unwrap();
        support::seed_into(
            &store,
            domain::MediaKind::Movie,
            2,
            1,
            library,
            "/query-work/movie",
        );
        done.send(()).unwrap();
    });
    let current = lease
        .with_connection(&coordination_budget(), |conn| {
            let (first, _) = library_item_page_on(conn, &query)?;
            start.send(()).unwrap();
            // Bounded: a writer that dies before committing must fail this read
            // instead of parking the lease thread forever.
            finished
                .recv_timeout(DEFAULT_GATE_TIMEOUT)
                .expect("the concurrent publish never finished");
            let (page, _) = library_item_page_on(conn, &query)?;
            let ids = page
                .items
                .iter()
                .map(|item| item.media_id)
                .collect::<Vec<_>>();
            let facts = library_item_facts_on(conn, library, &ids, None)?;
            Ok((first, page, facts))
        })
        .unwrap();
    writer.join().unwrap();
    assert_eq!(current.0.total, before.total);
    assert_eq!(current.1.total, before.total);
    assert_eq!(current.1.items, before.items);
    assert_eq!(current.2.len(), before.items.len());
    let next = lease
        .with_connection(&budget(), |conn| {
            library_item_page_on(conn, &query).map(|v| v.0)
        })
        .unwrap();
    assert_eq!(next.total, Some(before.total.unwrap() + 1));
}

#[test]
fn short_policy_interrupts_active_sql_then_allows_next_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let short = QueryBudget {
        deadline: Instant::now() + Duration::from_millis(1),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let result: Result<(), LibraryReadError> = store.with_query_budget(&short, |s| {
        s.library.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n",
            [], |r| r.get::<_, i64>(0),
        ).map_err(crate::StoreError::from)?;
        Ok(())
    });
    assert!(matches!(result, Err(LibraryReadError::Deadline)));
    assert_eq!(busy_timeout(&store.library), 5000);
    store
        .with_query_budget(&budget(), |s| {
            let count: i64 = s
                .library
                .query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get(0))
                .map_err(crate::StoreError::from)?;
            assert!(count > 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn nested_short_stage_rejection_keeps_outer_cancellation_active() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let outer = budget();
    let cancelled = Arc::clone(&outer.cancelled);
    let interrupted = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&interrupted);
    let result = store.with_query_budget(&outer, |s| {
        let nested = s.with_query_budget(&budget(), |_s| Ok(()));
        assert!(matches!(nested, Err(LibraryReadError::NestedBudget)));
        cancelled.store(true, Ordering::Release);
        let sql = s.library.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n",
            [], |r| r.get::<_, i64>(0),
        );
        observed.store(sql.is_err(), Ordering::Release);
        Ok(())
    });
    assert!(matches!(result, Err(LibraryReadError::Cancelled)));
    assert!(interrupted.load(Ordering::Acquire));
    assert_eq!(busy_timeout(&store.library), 5000);
}
