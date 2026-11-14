use domain::{JobDefId, JobId, MediaId};
use jobs::{JobKind, JobScope, NewDef, Queue, QueueRead};
use rusqlite::{Connection, OpenFlags, params};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn definition(queue: &Queue, name: &str) -> JobDefId {
    let media = MediaId::new();
    queue
        .create_def(NewDef {
            kind: JobKind::CatalogRefresh,
            scope: JobScope::Media(media),
            name: name.into(),
            enabled: true,
            schedule: None,
            payload: format!(r#"{{"media_id":"{media}"}}"#),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap()
        .id
}
fn child(
    conn: &Connection,
    def: Option<JobDefId>,
    status: &str,
    finished: Option<i64>,
    run: i64,
) -> JobId {
    let id = JobId::new();
    conn.execute("INSERT INTO jobs(id,def_id,kind,scope_key,payload,status,run_after,finished_at) VALUES(?1,?2,'catalog_refresh','global','{}',?3,?4,?5)",
        params![id.to_string(), def.map(|d|d.to_string()), status, run, finished]).unwrap();
    id
}
fn history(conn: &Connection, def: JobDefId, from: i64, to: i64) {
    conn.execute("WITH RECURSIVE n(x) AS (SELECT ?1 UNION ALL SELECT x+1 FROM n WHERE x<?2) INSERT INTO jobs(id,def_id,kind,scope_key,payload,status,run_after,finished_at) SELECT printf('%08x-0000-0000-0000-000000000000',x),?3,'invalid_old_kind','global','bad','succeeded',x,x FROM n",
        params![from,to,def.to_string()]).unwrap();
}

#[test]
fn latest_children_seek_work_is_bounded_when_terminal_history_grows_to_200k() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let queue = Queue::open(&path).unwrap();
    let full = definition(&queue, "full");
    let empty = definition(&queue, "empty");
    let live_only = definition(&queue, "live");
    let null_only = definition(&queue, "null");
    let fixture = Connection::open(&path).unwrap();
    let expected = child(&fixture, Some(full), "failed", Some(1_000_000), 20);
    child(&fixture, Some(full), "cancelled", Some(1_000_000), 10);
    child(&fixture, Some(full), "succeeded", None, 2_000_000);
    let live = child(&fixture, Some(live_only), "queued", None, 5);
    let null = child(&fixture, Some(null_only), "cancelled", None, 5);
    child(&fixture, None, "succeeded", Some(9_000_000), 9_000_000);
    history(&fixture, full, 1, 100);
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let read = QueueRead::new(&conn);
    let measure = || {
        count.store(0, Ordering::Relaxed);
        let rows = read
            .children_for_defs(&[full, empty, full, live_only, null_only])
            .unwrap();
        let work = count.load(Ordering::Relaxed);
        assert_eq!(
            rows.iter().map(|r| r.def_id).collect::<Vec<_>>(),
            vec![full, empty, live_only, null_only]
        );
        assert_eq!(rows[0].last.as_ref().unwrap().id, expected);
        assert!(rows[1].last.is_none() && rows[1].live.is_none());
        assert_eq!(rows[2].live.as_ref().unwrap().id, live);
        assert!(rows[2].last.is_none());
        assert_eq!(rows[3].last.as_ref().unwrap().id, null);
        for row in &rows {
            assert_eq!(
                row.last.as_ref().map(|j| j.id),
                read.get_last_child(row.def_id).unwrap().map(|j| j.id)
            );
        }
        work
    };
    let before = measure();
    history(&fixture, full, 101, 200_000);
    let after = measure();
    eprintln!("P5 fixed4 definitions,100→200000 historical rows: VM {before}→{after}");
    assert!(
        after <= before + 1000,
        "latest top1 work must not traverse terminal history: {before}→{after}"
    );
}

#[test]
fn exact_ties_agree_with_single_read_and_latest_corruption_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let queue = Queue::open(&path).unwrap();
    let def = definition(&queue, "ties");
    let conn = Connection::open(&path).unwrap();
    let a = child(&conn, Some(def), "succeeded", Some(100), 10);
    let b = child(&conn, Some(def), "cancelled", Some(100), 10);
    let single = queue.get_last_child(def).unwrap().unwrap().id;
    assert!([a, b].contains(&single));
    assert_eq!(
        queue.children_for_defs(&[def]).unwrap()[0]
            .last
            .as_ref()
            .unwrap()
            .id,
        single
    );
    conn.execute(
        "UPDATE jobs SET kind='broken-latest-kind' WHERE id=?1",
        [single.to_string()],
    )
    .unwrap();
    assert!(queue.get_last_child(def).is_err());
    assert!(queue.children_for_defs(&[def]).is_err());
}
