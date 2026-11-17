mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use support::library_presence::PresenceFixture;

#[derive(Default)]
struct CleanupGate {
    entered: tokio::sync::Notify,
    fired: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}

impl CleanupGate {
    fn pause(&self) {
        self.entered.notify_one();
        let released = self.released.lock().unwrap();
        let (released, timeout) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
            .unwrap();
        assert!(*released && !timeout.timed_out(), "cleanup gate timed out");
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

/// Pause the roots read after it has captured its result, before any sidecar IO.
/// The independent reader identifies the committed deletion; no production test
/// hook or database writer is involved in the gate.
fn pause_sidecar_cleanup(fix: &PresenceFixture, file: domain::LedgerId) -> Arc<CleanupGate> {
    let gate = Arc::new(CleanupGate::default());
    let pool = fix.state.store().reader_pool(store::Db::Library);
    let mut leases = (0..8)
        .map(|_| pool.try_checkout().unwrap())
        .collect::<Vec<_>>();
    for lease in &mut leases {
        let gate = gate.clone();
        let observer = rusqlite::Connection::open_with_flags(
            fix.dir.path().join("data/library.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let file = file.to_string();
        lease
            .with_connection(
                &store::QueryBudget {
                    deadline: Instant::now() + Duration::from_secs(5),
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
                |conn| {
                    let mut cleanup_roots = false;
                    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                        if gate.fired.load(Ordering::Acquire) {
                            return Authorization::Allow;
                        }
                        if matches!(
                            ctx.action,
                            AuthAction::Read {
                                table_name: "library_roots",
                                column_name: "root_id"
                            }
                        ) {
                            let present: bool = observer
                                .query_row(
                                    "SELECT EXISTS(SELECT 1 FROM library_files WHERE file_id=?1)",
                                    [&file],
                                    |row| row.get(0),
                                )
                                .unwrap();
                            cleanup_roots = !present;
                        }
                        if cleanup_roots
                            && matches!(ctx.action, AuthAction::Transaction { .. })
                            && !gate.fired.swap(true, Ordering::AcqRel)
                        {
                            gate.pause();
                        }
                        Authorization::Allow
                    }));
                    Ok(())
                },
            )
            .unwrap();
    }
    gate
}

async fn wait_removed(fix: &PresenceFixture, operation: &str) {
    loop {
        let (status, body) = fix
            .request(
                "GET",
                &format!("/api/v1/library-removals/{operation}"),
                Some(&fix.admin_token),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_ne!(body["data"]["state"], "failed", "{body}");
        if body["data"]["state"] == "succeeded" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn removal_waits_for_cleanup(fix: Arc<PresenceFixture>, full_scan: bool) {
    let (_, file) = fix.insert_media_and_ledger("missing/movie.mkv");
    let directory = file.path.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(file.path.with_extension("nfo"), b"movie metadata").unwrap();
    std::fs::write(directory.join("poster.jpg"), b"movie artwork").unwrap();
    let (status, _) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let gate = pause_sidecar_cleanup(&fix, file.file_id);
    let worker = fix.clone();
    let cleanup = tokio::spawn(async move {
        if full_scan {
            worker
                .request(
                    "POST",
                    &format!("/api/v1/libraries/{}/scan", worker.library_id),
                    Some(&worker.admin_token),
                )
                .await
        } else {
            worker
                .request(
                    "DELETE",
                    &format!("/api/v1/libraries/{}/missing-rows", worker.library_id),
                    Some(&worker.admin_token),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("presence must reach physical sidecar cleanup");
    assert!(
        fix.state
            .store()
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .is_none()
    );
    let (status, response) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let operation = response["data"]["operation_id"].as_str().unwrap();
    let premature =
        tokio::time::timeout(Duration::from_millis(150), wait_removed(&fix, operation)).await;
    let writer_available =
        fix.state
            .store()
            .writers()
            .with_writer_timeout(Duration::ZERO, store::Db::Library, |_| Ok(()));
    gate.release();
    cleanup.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), wait_removed(&fix, operation))
        .await
        .unwrap();
    assert!(
        writer_available.is_ok(),
        "cleanup must not hold the Library writer"
    );
    assert!(
        premature.is_err(),
        "Library removal succeeded before sidecar cleanup finished"
    );
    assert!(
        !directory.exists(),
        "admitted cleanup must finish before removal succeeds"
    );
    assert!(fix.root.exists(), "cleanup must preserve the Library root");
}

#[test]
fn removal_waits_for_missing_row_sidecar_cleanup() {
    let fix = Arc::new(PresenceFixture::new());
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(removal_waits_for_cleanup(fix, false));
}

#[test]
fn removal_waits_for_full_scan_sidecar_cleanup() {
    let fix = Arc::new(PresenceFixture::new());
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(removal_waits_for_cleanup(fix, true));
}
