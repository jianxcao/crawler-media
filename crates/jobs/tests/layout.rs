use jobs::{JobKind, Queue};
use rusqlite::Connection;

#[test]
fn independent_queue_creation_stamps_complete_layout_and_preserves_jobs_on_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let queue = Queue::open(&path).unwrap();
    let job = queue.enqueue(JobKind::Transfer, "keep", 10).unwrap();
    drop(queue);
    let conn = Connection::open(&path).unwrap();
    let identity: (i64, i64) = (
        conn.pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap(),
        conn.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap(),
    );
    assert_eq!(identity, (1129139252, 1));
    drop(conn);
    assert_eq!(Queue::open(&path).unwrap().get(job.id).unwrap(), Some(job));
}

#[test]
fn unsupported_queue_is_rejected_without_writes() {
    for version in [0, 2] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("jobs.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('keep');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(Queue::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("db-wal").exists());
    }
}

#[test]
fn current_queue_missing_required_objects_is_rejected_instead_of_repaired() {
    for object in [
        "INDEX jobs_last_def_idx",
        "TRIGGER trg_jobs_change_ins",
        "TABLE retention_cursors",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("jobs.db");
        drop(Queue::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!("DROP {object}")).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = Queue::open(&path)
            .err()
            .expect("damaged current layout rejected");
        assert!(
            error
                .to_string()
                .contains(object.split(' ').next_back().unwrap()),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn current_headers_do_not_allow_invalid_columns_or_missing_singleton() {
    for damage in [
        "ALTER TABLE jobs RENAME COLUMN progress TO obsolete_progress",
        "DELETE FROM job_change_state",
        "DROP INDEX jobs_last_def_idx; CREATE INDEX jobs_last_def_idx ON jobs(payload)",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("jobs.db");
        drop(Queue::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(damage).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(Queue::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn obsolete_queue_index_is_rejected_without_dropping_it_or_changing_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    drop(Queue::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE UNIQUE INDEX jobs_live_def ON jobs(def_id) WHERE status IN ('queued','running')",
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(Queue::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
