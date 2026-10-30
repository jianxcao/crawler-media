use super::app_writer::HeldAppWriter;
use super::fixture::Fixture;
use super::requests::HeldWriter;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn beat(path: &Path) -> i64 {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT heartbeat_at FROM instance_lock WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

async fn changed(path: &Path, previous: i64, deadline: Duration) -> bool {
    tokio::time::timeout(deadline, async {
        loop {
            if beat(path) > previous {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

struct LeaseTimers(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl LeaseTimers {
    fn fast() -> Self {
        let overrides = [
            ("CRAWLER_MEDIA_LEASE_HEARTBEAT_MS", "50"),
            ("CRAWLER_MEDIA_LEASE_TTL_MS", "30000"),
        ];
        let previous = overrides
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        for (name, value) in overrides {
            // SAFETY: this binary has one lease test; the values only shorten
            // heartbeat cadence and retain a TTL longer than the test obstruction.
            unsafe { std::env::set_var(name, value) };
        }
        Self(previous)
    }
}

impl Drop for LeaseTimers {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            // SAFETY: restore the sole lease test's process-local timer overrides.
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_uses_the_shared_app_writer_and_remains_independent_of_outer_store() {
    let _timers = LeaseTimers::fast();
    let fixture = Fixture::new();
    let path: PathBuf = fixture.state.store().lock().app_db_path().to_owned();
    let loop_handle = api::job_loop::spawn_job_loop(fixture.state.clone());
    assert!(!loop_handle.is_read_only());
    let previous = beat(&path);
    let outer = HeldWriter::acquire(&fixture.state);
    let refreshed_under_outer = changed(&path, previous, Duration::from_millis(500)).await;
    outer.finish();
    assert!(
        refreshed_under_outer,
        "heartbeat queued behind the outer Store"
    );
    let app = HeldAppWriter::acquire(&fixture.state);
    let previous = beat(&path);
    let escaped_app = changed(&path, previous, Duration::from_millis(180)).await;
    drop(app);
    assert!(!escaped_app, "heartbeat used another app writer connection");
    assert!(changed(&path, previous, Duration::from_secs(2)).await);
    assert!(
        !loop_handle.is_read_only(),
        "brief app contention must not lose the lease"
    );
    loop_handle.shutdown().await.unwrap();
}
