use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use jobs::{JobIdentity, JobKind, Queue};
use media::response_cache::{self, CacheSource};
use rusqlite::{Connection, params};
use store::Store;

const CHILD_DIR: &str = "CRAWLER_SQLITE_STARTUP_CHILD_DIR";
const WORKERS: usize = 8;

#[test]
fn concurrent_first_opens_in_fresh_processes() {
    let root = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    for iteration in 0..16 {
        let dir = root.path().join(iteration.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        if iteration % 2 == 1 {
            seed_existing_databases(&dir);
        }
        let stdout_path = root.path().join(format!("{iteration}.stdout"));
        let stderr_path = root.path().join(format!("{iteration}.stderr"));
        let mut child = Command::new(&executable)
            .args([
                "--exact",
                "sqlite_first_open_child",
                "--test-threads=8",
                "--nocapture",
            ])
            .env(CHILD_DIR, &dir)
            .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => {
                    let reason = match result {
                        Ok(None) => "timed out after 30 seconds".to_owned(),
                        Err(error) => format!("failed to wait: {error}"),
                        Ok(Some(_)) => unreachable!(),
                    };
                    let kill = child.kill();
                    let reap = child.wait();
                    break Err(format!("{reason}; kill: {kill:?}; reap: {reap:?}"));
                }
            }
        };
        let stdout = std::fs::read_to_string(&stdout_path)
            .unwrap_or_else(|error| format!("could not read stdout: {error}"));
        let stderr = std::fs::read_to_string(&stderr_path)
            .unwrap_or_else(|error| format!("could not read stderr: {error}"));
        let status = outcome.unwrap_or_else(|error| {
            panic!("startup subprocess {iteration} {error}\nstdout:\n{stdout}\nstderr:\n{stderr}")
        });
        assert!(
            status.success(),
            "startup subprocess {iteration} failed: {}\nstdout:\n{}\nstderr:\n{}",
            status,
            stdout,
            stderr,
        );
        assert!(
            stdout.contains("1 passed"),
            "startup subprocess {iteration} did not run the child test:\n{}\n{}",
            stdout,
            stderr,
        );
    }
}

fn seed_existing_databases(dir: &Path) {
    for worker in 0..WORKERS {
        let path = dir.join(worker.to_string());
        match worker % 4 {
            0 => drop(Store::open(&path).unwrap()),
            1 => drop(Queue::open(path.join("jobs.db")).unwrap()),
            2 => drop(response_cache::open(path.join("cache.db")).unwrap()),
            _ => {
                std::fs::create_dir_all(&path).unwrap();
                drop(Connection::open(path.join("raw.db")).unwrap());
            }
        }
    }
}

#[test]
fn sqlite_first_open_child() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let barrier = Arc::new(Barrier::new(WORKERS));
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let barrier = Arc::clone(&barrier);
            let path = dir.join(worker.to_string());
            scope.spawn(move || {
                barrier.wait();
                match worker % 4 {
                    0 => check_store(&path, worker),
                    1 => check_queue(&path, worker),
                    2 => check_cache(&path, worker),
                    _ => check_raw(&path, worker),
                }
            });
        }
    });
}

fn check_store(path: &Path, worker: usize) {
    let store = Store::open(path).unwrap();
    assert_eq!(store.get_setting("startup-worker").unwrap(), None);
    let value = worker.to_string();
    store.put_setting("startup-worker", &value).unwrap();
    assert_eq!(store.get_setting("startup-worker").unwrap(), Some(value));
}

fn check_queue(path: &Path, worker: usize) {
    let queue = Queue::open(path.join("jobs.db")).unwrap();
    let identity = JobIdentity::global(JobKind::Transfer);
    assert!(queue.jobs_for_identity(identity, 10).unwrap().is_empty());
    let payload = worker.to_string();
    let job = queue.enqueue(identity, &payload, 0).unwrap();
    assert_eq!(queue.get(job.id).unwrap().unwrap().payload, payload);
    assert_eq!(queue.jobs_for_identity(identity, 10).unwrap().len(), 1);
}

fn check_cache(path: &Path, worker: usize) {
    let conn = response_cache::open(path.join("cache.db")).unwrap();
    assert!(
        response_cache::get(&conn, CacheSource::Tmdb, "startup")
            .unwrap()
            .is_none()
    );
    let body = worker.to_string();
    response_cache::put(&conn, CacheSource::Tmdb, "startup", &body, 1, None).unwrap();
    assert_eq!(
        response_cache::get(&conn, CacheSource::Tmdb, "startup")
            .unwrap()
            .unwrap()
            .body,
        body
    );
}

fn check_raw(path: &Path, worker: usize) {
    std::fs::create_dir_all(path).unwrap();
    let conn = Connection::open(path.join("raw.db")).unwrap();
    conn.execute("CREATE TABLE startup (value INTEGER NOT NULL)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO startup (value) VALUES (?1)",
        params![worker as i64],
    )
    .unwrap();
    let value: i64 = conn
        .query_row("SELECT value FROM startup", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, worker as i64);
}
