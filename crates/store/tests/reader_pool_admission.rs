//! Public reader admission must preserve capacity across mixed waiters and cancellation.
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};
use store::{Db, LibraryReadError, QueryBudget, ReaderPool, Store};
const WAIT: Duration = Duration::from_secs(3);
fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + WAIT,
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}
fn pool(store: &Store) -> Arc<ReaderPool> {
    store
        .configure_reader_pool_size(Db::Library, NonZeroUsize::new(1).unwrap())
        .unwrap();
    store.reader_pool(Db::Library)
}
struct Notified(AtomicBool);
impl Wake for Notified {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}
fn wait_for_queue(pool: &ReaderPool, count: usize) {
    let deadline = Instant::now() + WAIT;
    while pool.waiting() != count {
        assert!(
            Instant::now() < deadline,
            "reader queue never reached {count} tickets"
        );
        std::thread::yield_now();
    }
}
#[test]
fn each_database_uses_one_pool_with_its_capacity_and_query_only_policy() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (db, capacity) in [
        (Db::Library, 8),
        (Db::App, 4),
        (Db::Subscribe, 2),
        (Db::Cache, 2),
    ] {
        let pool = store.reader_pool(db);
        assert!(Arc::ptr_eq(&pool, &store.reader_pool(db)));
        let mut leases = (0..capacity)
            .map(|_| pool.try_checkout().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Busy)));
        let lease = &mut leases[0];
        let policy = lease
            .with_connection(&budget(), |conn| {
                Ok(conn.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))?)
            })
            .unwrap();
        assert_eq!(policy, 1);
        assert!(
            lease
                .with_connection(&budget(), |conn| {
                    conn.execute_batch("CREATE TEMP TABLE forbidden(value)")?;
                    Ok(())
                })
                .is_err()
        );
        drop(leases);
        assert!(
            pool.try_checkout().is_ok(),
            "returned {db:?} reader was lost"
        );
    }
}
#[test]
fn sync_and_async_waiters_share_fifo_and_an_unpolled_assignment_can_be_cancelled() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = pool(&store);
    let held = pool.try_checkout().unwrap();
    let notified = Arc::new(Notified(AtomicBool::new(false)));
    let waker = Waker::from(notified.clone());
    let mut cx = Context::from_waker(&waker);
    let mut abandoned = Box::pin(pool.checkout_async(WAIT));
    assert!(abandoned.as_mut().poll(&mut cx).is_pending());
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let sync_pool = pool.clone();
    let sync = std::thread::spawn(move || {
        let lease = sync_pool.checkout(WAIT).unwrap();
        acquired_tx.send(()).unwrap();
        release_rx.recv_timeout(WAIT).unwrap();
        drop(lease);
    });
    wait_for_queue(&pool, 2);
    let mut last = Box::pin(pool.checkout_async(WAIT));
    assert!(last.as_mut().poll(&mut cx).is_pending());
    drop(held);
    assert!(notified.0.load(Ordering::Acquire));
    assert!(
        matches!(pool.try_checkout(), Err(LibraryReadError::Busy)),
        "new arrivals bypassed FIFO"
    );
    // The queue head was woken for available capacity but never polled again.
    drop(abandoned);
    acquired_rx
        .recv_timeout(WAIT)
        .expect("cancelled head stranded the returned slot");
    assert!(
        last.as_mut().poll(&mut cx).is_pending(),
        "async waiter bypassed its earlier blocking peer"
    );
    release_tx.send(()).unwrap();
    sync.join().unwrap();
    let lease = match last.as_mut().poll(&mut cx) {
        Poll::Ready(Ok(lease)) => lease,
        _ => panic!("FIFO tail did not receive the returned reader"),
    };
    drop(lease);
    assert!(pool.try_checkout().is_ok());
}
#[test]
fn cancelling_queued_async_waiter_and_closing_pool_release_all_waiters() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = pool(&store);
    let held = pool.try_checkout().unwrap();
    let waker = Waker::from(Arc::new(Notified(AtomicBool::new(false))));
    let mut cx = Context::from_waker(&waker);
    let mut cancelled = Box::pin(pool.checkout_async(WAIT));
    assert!(cancelled.as_mut().poll(&mut cx).is_pending());
    drop(cancelled);
    let mut asynchronous = Box::pin(pool.checkout_async(WAIT));
    assert!(asynchronous.as_mut().poll(&mut cx).is_pending());
    let sync_pool = pool.clone();
    let blocking = std::thread::spawn(move || sync_pool.checkout(WAIT));
    wait_for_queue(&pool, 2);
    pool.close();
    assert!(matches!(
        asynchronous.as_mut().poll(&mut cx),
        Poll::Ready(Err(LibraryReadError::Closed))
    ));
    assert!(matches!(
        blocking.join().unwrap(),
        Err(LibraryReadError::Closed)
    ));
    drop(asynchronous);
    drop(held);
    assert_eq!(pool.available(), 0);
    assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Closed)));
}

#[test]
fn shared_fifo_rejects_a_fifth_waiter_and_recovers_after_cancellation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pool = pool(&store);
    let held = pool.try_checkout().unwrap();
    let waker = Waker::from(Arc::new(Notified(AtomicBool::new(false))));
    let mut cx = Context::from_waker(&waker);
    let mut waiting = (0..4)
        .map(|_| Box::pin(pool.checkout_async(WAIT)))
        .collect::<Vec<_>>();
    for waiter in &mut waiting {
        assert!(waiter.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(pool.waiting(), 4);
    let mut refused = Box::pin(pool.checkout_async(WAIT));
    assert!(matches!(
        refused.as_mut().poll(&mut cx),
        Poll::Ready(Err(LibraryReadError::Busy))
    ));
    assert_eq!(pool.waiting(), 4);
    drop(waiting.remove(0));
    let mut replacement = Box::pin(pool.checkout_async(WAIT));
    assert!(replacement.as_mut().poll(&mut cx).is_pending());
    assert_eq!(pool.waiting(), 4);
    drop(held);
    let lease = match waiting[0].as_mut().poll(&mut cx) {
        Poll::Ready(Ok(lease)) => lease,
        _ => panic!("FIFO did not admit the first surviving waiter"),
    };
    assert!(replacement.as_mut().poll(&mut cx).is_pending());
    drop(lease);
    drop(waiting);
    let lease = match replacement.as_mut().poll(&mut cx) {
        Poll::Ready(Ok(lease)) => lease,
        _ => panic!("cancellation stranded the replacement waiter"),
    };
    drop(lease);
    assert_eq!(pool.waiting(), 0);
    assert_eq!(pool.available(), 1);
}
