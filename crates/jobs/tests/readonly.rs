use jobs::{JobKind, NewDef, Queue, QueueRead};

#[test]
fn queue_reader_serves_committed_jobs_while_the_writer_has_an_open_transaction() {
    let temporary = tempfile::tempdir_in("/tmp").unwrap();
    let path = temporary.path().join("jobs.db");
    let writer = Queue::open(&path).unwrap();
    let def = writer
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "Transfer".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let child = writer.ensure_scheduled_for(0, def.id).unwrap().remove(0);
    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    connection.pragma_update(None, "query_only", true).unwrap();
    jobs::schema::validate_layout(&connection).unwrap();
    let reader = QueueRead::new(&connection);
    let transaction = rusqlite::Connection::open(&path).unwrap();
    transaction
        .execute_batch("BEGIN IMMEDIATE; UPDATE jobs SET status='succeeded',finished_at=42;")
        .unwrap();
    assert_eq!(reader.def(def.id).unwrap().unwrap().name, "Transfer");
    assert_eq!(reader.list_defs().unwrap().len(), 1);
    assert_eq!(
        reader.get(child.id).unwrap().unwrap().status,
        jobs::JobStatus::Queued
    );
    assert_eq!(reader.change_state().unwrap().finished_count, 0);
    assert!(
        reader.children_for_defs(&[def.id]).unwrap()[0]
            .live
            .is_some()
    );
    transaction.execute_batch("COMMIT").unwrap();
    assert_eq!(
        reader.get(child.id).unwrap().unwrap().status,
        jobs::JobStatus::Succeeded
    );
    assert_eq!(reader.count_finished_since(0).unwrap(), 1);
    assert!(
        reader.children_for_defs(&[def.id]).unwrap()[0]
            .last
            .is_some()
    );
}

#[test]
fn queue_reader_previews_retention_without_acquiring_a_writer_transaction() {
    let temporary = tempfile::tempdir_in("/tmp").unwrap();
    let path = temporary.path().join("jobs.db");
    let writer = Queue::open(&path).unwrap();
    let job = writer.enqueue(JobKind::Transfer, "{}", 0).unwrap();
    writer.claim(1, 1).unwrap();
    writer.succeed(job.id, 2).unwrap();
    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    connection.pragma_update(None, "query_only", true).unwrap();
    let blocked = rusqlite::Connection::open(&path).unwrap();
    blocked.execute_batch("BEGIN IMMEDIATE").unwrap();
    let preview = QueueRead::new(&connection)
        .preview_terminal_window("preview", 100, 100)
        .unwrap();
    assert_eq!(preview.scanned, 1);
    assert_eq!(preview.candidates, 1);
    assert_eq!(preview.deleted, 0);
    assert_eq!(
        writer.get(job.id).unwrap().unwrap().status,
        jobs::JobStatus::Succeeded
    );
    let cursors: i64 = connection
        .query_row("SELECT count(*) FROM retention_cursors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(cursors, 0);
    blocked.execute_batch("ROLLBACK").unwrap();
}
