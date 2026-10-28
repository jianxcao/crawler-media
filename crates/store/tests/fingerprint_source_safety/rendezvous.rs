//! Bounded rendezvous between a capture thread and a deleting thread (review F5).
//!
//! The old round used a `Barrier`: the deleting side panicked on any non-`Busy` error
//! and never reached its second `wait`, so `thread::scope` joined a capture thread that
//! would never be released — one test error became a permanent hang. Every wait here has
//! a deadline, the delete side always releases the capture before reporting its own
//! error, and the capture side reports that it stopped, so a failure is reported as
//! itself instead of as a hang.
use std::sync::mpsc;
use std::time::{Duration, Instant};

use store::Store;
use store::test_support::FixtureFile;

use super::{capture_result, row, sample_query};

/// A round may take as long as one SQLite busy window plus scheduling; well past that,
/// the round is broken, not slow.
const ROUND_DEADLINE: Duration = Duration::from_secs(10);

fn rows(store: &Store, root: &std::path::Path, count: usize) -> Vec<FixtureFile> {
    (0..count)
        .map(|index| {
            let mut row = row(store, root, domain::MediaId::new());
            row.path = root
                .join(format!("episode-{index}.mkv"))
                .to_string_lossy()
                .into_owned();
            std::fs::write(&row.path, b"fixture").unwrap();
            store::test_support::publish_fixture_file(store, &row).unwrap();
            row
        })
        .collect()
}

/// Wait for one rendezvous signal, or give up at the deadline.
fn await_signal(receiver: &mpsc::Receiver<()>, deadline: Duration) -> Result<(), String> {
    receiver
        .recv_timeout(deadline)
        .map_err(|error| format!("rendezvous signal did not arrive: {error}"))
}

/// Wait for the lock, then delete: SQLite may answer `Busy` while the capture holds
/// the write lock, so the retry is bounded by an absolute deadline instead of a count.
fn delete_with_busy_retry(deleting: &Store, path: &str) -> Result<(), store::StoreError> {
    let deadline = Instant::now() + ROUND_DEADLINE;
    loop {
        match deleting.delete_ledger_path(path) {
            Err(store::StoreError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)))
                if failure.code == rusqlite::ErrorCode::DatabaseBusy
                    && Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            other => return other,
        }
    }
}

/// Run one round: wait for the capture, delete, then release the capture before
/// reporting the delete's own outcome.
///
/// `delete` is a parameter so a test can inject a non-`Busy` failure and check that the
/// error path still releases the capture instead of parking the scope join forever.
fn round(
    ready_rx: &mpsc::Receiver<()>,
    release_tx: &mpsc::Sender<()>,
    delete: impl FnOnce() -> Result<(), store::StoreError>,
) -> Result<(), store::StoreError> {
    await_signal(ready_rx, ROUND_DEADLINE).expect("capture must become ready");
    let deleted = delete();
    // Release first: the capture waits on this round, not on the delete's outcome.
    let released = release_tx.send(()).is_ok();
    deleted?;
    assert!(released, "capture cancelled before release");
    Ok(())
}

#[test]
fn independent_store_handles_cannot_reinsert_samples_while_ledger_is_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let deleting = Store::open(temp.path()).unwrap();
    let capturing = Store::open(temp.path()).unwrap();
    let rows = rows(&deleting, temp.path(), 32);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let rows = &rows;
        scope.spawn(move || {
            for row in rows.iter() {
                let (attempt, sample) = capture_result(row);
                if ready_tx.send(()).is_err() {
                    return;
                }
                // A concurrent deletion may reject the write or invalidate a SQLite read
                // snapshot. Either result must leave no sample once deletion commits.
                let _ =
                    capturing.complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path);
                if release_rx.recv_timeout(ROUND_DEADLINE).is_err() {
                    return;
                }
            }
            let _ = done_tx.send(());
        });
        let mut failure = None;
        for row in rows {
            let path = row.path.as_str();
            if let Err(error) = round(&ready_rx, &release_tx, || {
                delete_with_busy_retry(&deleting, path)
            }) {
                failure = Some(error);
                break;
            }
        }
        // Closing the channel also cancels the capture on every error path.
        drop(release_tx);
        let stopped = await_signal(&done_rx, ROUND_DEADLINE);
        if let Some(error) = failure {
            panic!("delete failed: {error}");
        }
        assert!(stopped.is_ok(), "capture did not stop before the deadline");
    });
    for row in &rows {
        let (_, sample) = capture_result(row);
        assert!(deleting.file_coverage_by_path(&row.path).unwrap().is_none());
        assert!(
            deleting
                .find_covering_fingerprint_samples(&sample_query(&sample))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn a_delete_error_releases_the_capture_and_reports_itself() {
    let temp = tempfile::tempdir().unwrap();
    let deleting = Store::open(temp.path()).unwrap();
    let capturing = Store::open(temp.path()).unwrap();
    let rows = rows(&deleting, temp.path(), 1);
    let row = &rows[0];
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::scope(|scope| {
        let captured = row;
        scope.spawn(move || {
            let (attempt, sample) = capture_result(captured);
            if ready_tx.send(()).is_err() {
                return;
            }
            let _ = capturing.complete_fingerprint_attempt_for_source(
                &attempt,
                &sample,
                &captured.path,
            );
            // Report whether the round released us, so the assertion can tell a
            // released capture from a deadline timeout.
            let released = release_rx.recv_timeout(ROUND_DEADLINE).is_ok();
            let _ = done_tx.send(released);
        });
        let outcome = round(&ready_rx, &release_tx, || {
            Err(store::StoreError::Invalid("injected delete failure".into()))
        });
        assert!(outcome.is_err(), "the injected failure must be reported");
        let released = done_rx.recv_timeout(ROUND_DEADLINE);
        assert_eq!(
            released.as_ref().ok(),
            Some(&true),
            "capture must be released before the error is reported"
        );
    });
    assert!(
        started.elapsed() < ROUND_DEADLINE,
        "the round must be bounded"
    );
    // Nothing was deleted, so the row is still the authoritative record.
    assert!(deleting.file_coverage_by_path(&row.path).unwrap().is_some());
}

#[test]
fn a_capture_that_never_signals_ready_fails_within_the_deadline() {
    let (ready_tx, ready_rx) = mpsc::channel();
    // The sender is alive but silent: exactly the "worker never became ready" case.
    let _silent = ready_tx;
    let started = Instant::now();
    let outcome = await_signal(&ready_rx, Duration::from_millis(50));
    assert!(outcome.is_err(), "a silent capture must not wait forever");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_dropped_peer_cancels_the_wait_immediately() {
    let (ready_tx, ready_rx) = mpsc::channel();
    drop(ready_tx);
    // A cancelled peer (thread gone or error path) is reported at once, not at the deadline.
    let started = Instant::now();
    assert!(await_signal(&ready_rx, ROUND_DEADLINE).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}
