//! Opening a reader must not block async admission, cancellation or other tasks.
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{Arc, mpsc},
    task::{Context, Waker},
    time::{Duration, Instant},
};

use rusqlite::Connection;
use store::{LibraryReadError, QueryBudget, ReaderPool};

const WAIT: Duration = Duration::from_secs(3);

struct LockedPool {
    _dir: tempfile::TempDir,
    pool: Arc<ReaderPool>,
    release: Option<mpsc::Sender<()>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl LockedPool {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("readers.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE facts(value INTEGER);
                 INSERT INTO facts VALUES(42);",
            )
            .unwrap();
        drop(writer);
        let pool = Arc::new(
            ReaderPool::open(path.clone(), NonZeroUsize::new(1).unwrap(), "test", |_| {
                Ok(())
            })
            .unwrap(),
        );
        // The next admission must open a real connection, as after a scan refresh.
        pool.invalidate_planner();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let conn = Connection::open(path).unwrap();
            conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
                .unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(WAIT);
            conn.execute_batch("COMMIT").unwrap();
        });
        locked_rx.recv_timeout(WAIT).unwrap();
        Self {
            _dir: dir,
            pool,
            release: Some(release_tx),
            writer: Some(writer),
        }
    }

    fn unlock(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(writer) = self.writer.take() {
            writer.join().unwrap();
        }
    }
}

impl Drop for LockedPool {
    fn drop(&mut self) {
        self.unlock();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn opening_reader_obeys_async_timeout_and_does_not_stall_other_tasks() {
    let mut fixture = LockedPool::new();
    let peer = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
    });
    let result = fixture.pool.checkout_async(Duration::from_millis(40)).await;
    assert!(
        matches!(result, Err(LibraryReadError::Busy)),
        "opening a locked database bypassed the async admission timeout"
    );
    peer.await.unwrap();

    fixture.unlock();
    // The timed-out opener still owns its capacity until its connection is
    // physically returned; a subsequent borrower must recover that same slot.
    let mut lease = fixture.pool.checkout_async(WAIT).await.unwrap();
    let value = lease
        .with_connection(
            &QueryBudget {
                deadline: Instant::now() + WAIT,
                cancelled: Default::default(),
            },
            |conn| Ok(conn.query_row("SELECT value FROM facts", [], |row| row.get::<_, i64>(0))?),
        )
        .unwrap();
    assert_eq!(value, 42);
    assert!(matches!(
        fixture.pool.try_checkout(),
        Err(LibraryReadError::Busy)
    ));
    drop(lease);
    assert_eq!(fixture.pool.available(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_an_opening_keeps_its_slot_until_the_connection_returns() {
    let mut fixture = LockedPool::new();
    let mut opening = Box::pin(fixture.pool.checkout_async(WAIT));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(opening.as_mut().poll(&mut cx).is_pending());
    drop(opening);
    assert!(matches!(
        fixture.pool.try_checkout(),
        Err(LibraryReadError::Busy)
    ));

    fixture.unlock();
    let lease = fixture.pool.checkout_async(WAIT).await.unwrap();
    assert!(matches!(
        fixture.pool.try_checkout(),
        Err(LibraryReadError::Busy)
    ));
    drop(lease);
    assert_eq!(fixture.pool.available(), 1);
    assert_eq!(fixture.pool.waiting(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn closing_pool_wakes_an_async_borrower_while_its_connection_opens() {
    let mut fixture = LockedPool::new();
    let pool = fixture.pool.clone();
    let opening = tokio::spawn(async move { pool.checkout_async(WAIT).await });
    tokio::task::yield_now().await;
    assert!(!opening.is_finished());

    fixture.pool.close();
    let result = tokio::time::timeout(Duration::from_millis(40), opening)
        .await
        .expect("closing the pool did not wake its opening borrower")
        .unwrap();
    assert!(matches!(result, Err(LibraryReadError::Closed)));
    fixture.unlock();
    assert_eq!(fixture.pool.available(), 0);
    assert!(matches!(
        fixture.pool.try_checkout(),
        Err(LibraryReadError::Closed)
    ));
}
