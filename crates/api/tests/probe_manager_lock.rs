#[path = "probe_manager_lock/support.rs"]
mod support;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::StatusCode;

/// Real async handler: progress -> missing duration -> ensure_probe_enqueued.
/// Construct outside Tokio so this finite fixture never starts actual probe workers.
#[test]
fn probe_round_does_not_stall_runtime_while_writer_is_held() {
    let fixture = support::Fixture::new();
    let writer = fixture.state.store();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = writer.lock();
        held_tx.send(()).unwrap();
        // The fallback lets the unfixed handler unblock and report its timer
        // failure instead of deadlocking the single-threaded test runtime.
        let _ = release_rx.recv_timeout(Duration::from_secs(3));
    });
    held_rx.recv().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = fixture.state.probe.clone();
    let previous = probe.assessments.load(Ordering::SeqCst);
    let (elapsed, status, body) = runtime.block_on(async move {
        let began = Instant::now();
        let request = tokio::spawn(async move { fixture.progress().await });
        // The public diagnostic ensures this HTTP request really reaches probe
        // assessment. Counting from before that point also catches a blocked
        // runtime that cannot even resume this observer until the writer releases.
        while probe.assessments.load(Ordering::SeqCst) == previous {
            if request.is_finished() {
                panic!(
                    "heartbeat exited before probe admission: {:?}",
                    request.await.unwrap()
                );
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let elapsed = began.elapsed();
        // Let the heartbeat's own bounded progress write complete after the
        // responsiveness observation, without making contention imply HTTP 503.
        let _ = release_tx.send(());
        let (status, body) = request.await.unwrap();
        (elapsed, status, body)
    });
    holder.join().unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        elapsed < Duration::from_millis(200),
        "probe admission stalled the only Tokio worker for {elapsed:?}"
    );
}
/// The synchronous admission API is also used by the blocking scan dispatcher.
#[test]
fn probe_admission_returns_busy_before_the_writer_is_released_without_a_late_job() {
    let fixture = support::Fixture::new();
    let shared = fixture.state.store();
    let row = shared.lock().list_file_coverage().unwrap().remove(0);
    let file_id = row.file.file_id.to_string();
    let probe = fixture.state.probe.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer = shared.clone();
    let holder = std::thread::spawn(move || {
        let _guard = writer.lock();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(3));
    });
    held_rx.recv().unwrap();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let caller = std::thread::spawn(move || {
        let outcome =
            probe.request_probe(&row, api::probe_manager::policy::ProbeRequestOrigin::Detail);
        finished_tx.send(outcome).unwrap();
    });
    let outcome = finished_rx.recv_timeout(Duration::from_millis(1_500));
    let _ = release_tx.send(());
    holder.join().unwrap();
    caller.join().unwrap();
    assert!(
        matches!(outcome, Ok(Err(store::StoreError::Io(ref error)))
            if error.kind() == std::io::ErrorKind::WouldBlock),
        "admission must return temporary busy within its writer budget: {outcome:?}"
    );
    assert!(
        shared
            .lock()
            .active_probe_unit_for_ledger(&file_id)
            .unwrap()
            .is_none(),
        "a timed-out admission must not enqueue after its caller returns"
    );
}
