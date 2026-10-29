//! The seam itself: a contended `Store` lock answers "busy" inside the bound, a
//! real read failure keeps its own error, and a worker that finishes in time is
//! untouched.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;

use super::{STORE_READ_WAIT, STORE_WRITE_WAIT, StoreCallError, store_read_handle};

fn store(root: &std::path::Path) -> Arc<parking_lot::Mutex<crate::Store>> {
    Arc::new(parking_lot::Mutex::new(
        crate::Store::open(root.join("data")).unwrap(),
    ))
}

/// Another thread holds the lock for far longer than the deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contended_lock_is_busy_within_the_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = store(tmp.path());
    let held = handle.clone();
    let locked = std::thread::spawn(move || {
        let _guard = held.lock();
        std::thread::sleep(Duration::from_millis(1500));
    });
    std::thread::sleep(Duration::from_millis(20));

    let deadline = Duration::from_millis(100);
    let started = Instant::now();
    let outcome = store_read_handle(handle, deadline, |store| {
        store.default_library(domain::MediaKind::Movie)
    })
    .await;
    let waited = started.elapsed();

    assert!(
        matches!(outcome, Err(StoreCallError::Busy)),
        "a contended lock must surface as Busy"
    );
    assert!(
        waited < Duration::from_millis(600),
        "the seam waited {waited:?} for a {deadline:?} deadline"
    );
    locked.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_maps_to_503_with_retry_after() {
    let response = StoreCallError::Busy.response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "a contended lock must tell the client to retry"
    );
    assert_eq!(
        StoreCallError::Deadline.response().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_read_failure_keeps_the_handlers_own_error() {
    use axum::response::IntoResponse;

    let tmp = tempfile::tempdir().unwrap();
    let handle = store(tmp.path());
    let outcome = store_read_handle(handle, Duration::from_millis(500), |_store| {
        Err::<(), _>(crate::http::err(
            StatusCode::NOT_FOUND,
            "playback.item_missing",
            "条目不存在",
        ))
    })
    .await;

    match outcome {
        Ok(Err(response)) => assert_eq!(response.status(), StatusCode::NOT_FOUND),
        _ => panic!("a handler error must pass through, not become busy"),
    }
    assert_eq!(
        crate::http::err(StatusCode::INTERNAL_SERVER_ERROR, "x", "y")
            .into_response()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_store_answers_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = store(tmp.path());
    let deadline = Duration::from_secs(1);
    let started = Instant::now();
    let outcome = store_read_handle(handle, deadline, |store| {
        store.default_library(domain::MediaKind::Movie)
    })
    .await;
    assert!(outcome.is_ok(), "an idle store must be read");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "an idle read took {:?}",
        started.elapsed()
    );
}

/// 预算按方向分离是策略，不是巧合（Task 27）：读要能盖住一个长批，写要早退早重试。
/// 谁把两者并回一个常量，这条会红。
#[test]
fn read_budget_covers_a_long_batch_and_write_budget_stays_short() {
    assert!(
        STORE_READ_WAIT >= Duration::from_secs(3),
        "读预算要覆盖一个长批（10k 集扫描实测 ~1.3 s/批），实际 {STORE_READ_WAIT:?}"
    );
    assert_eq!(
        STORE_WRITE_WAIT,
        Duration::from_secs(1),
        "写预算刻意保持短：等满再报 Deadline 等于花掉耐心却说不清结果"
    );
    assert!(
        STORE_WRITE_WAIT < STORE_READ_WAIT,
        "写预算必须短于读预算，否则两个入口就没有区分的理由"
    );
}

#[tokio::test]
async fn cancelled_read_does_not_run_after_waiting_for_a_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = store(tmp.path());
    let readers = Arc::new(crate::store_readers::StoreReaders::open(&handle.lock(), 1).unwrap());
    let held = readers.checkout(Duration::ZERO).unwrap();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = ran.clone();
    let pool = readers.clone();
    let waiting = tokio::spawn(super::store_read_readers(
        pool,
        Duration::from_secs(1),
        move |_| observed.store(true, std::sync::atomic::Ordering::SeqCst),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "a cancelled waiter must leave the queue without taking a connection"
    );
}

#[tokio::test]
async fn pooled_read_waits_for_return_then_reads_without_the_writer_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let writer = store(tmp.path());
    let readers = Arc::new(crate::store_readers::StoreReaders::open(&writer.lock(), 1).unwrap());
    let held = readers.checkout(Duration::ZERO).unwrap();
    let queued = tokio::spawn(super::store_read_readers(
        readers,
        Duration::from_secs(1),
        |store| store.list_libraries(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !queued.is_finished(),
        "a normal read must wait for a reader"
    );
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _writer = writer.lock();
        locked_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    });
    locked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    drop(held);
    let outcome = queued.await;
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let libraries = outcome.unwrap().unwrap().unwrap();
    assert!(!libraries.is_empty());
}

#[tokio::test]
async fn pool_wait_expires_with_busy_within_the_request_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let writer = store(tmp.path());
    let readers = Arc::new(crate::store_readers::StoreReaders::open(&writer.lock(), 1).unwrap());
    let _held = readers.checkout(Duration::ZERO).unwrap();
    let deadline = Duration::from_millis(100);
    let started = Instant::now();
    let outcome: Result<(), _> =
        super::store_read_readers(readers, deadline, |_| panic!("pool remains full")).await;
    assert!(matches!(outcome, Err(StoreCallError::Busy)));
    assert!(started.elapsed() < Duration::from_millis(500));
}
