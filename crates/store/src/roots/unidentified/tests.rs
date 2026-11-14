use std::cell::Cell;

use rusqlite::{
    StatementStatus,
    trace::{TraceEvent, TraceEventCodes},
};

use crate::Store;

thread_local! { static VM: Cell<u64> = const { Cell::new(0) }; }

fn observe(event: TraceEvent<'_>) {
    if let TraceEvent::Profile(statement, _) = event {
        VM.with(|total| {
            total.set(total.get() + statement.get_status(StatementStatus::VmStep) as u64)
        });
    }
}

/// Exercise the public bounded drain, measuring every SQL statement in that window.
fn window_work(preceding: usize, unrelated: usize) -> u64 {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == library.id.as_str())
        .unwrap();
    store.with_writer(crate::Db::Library, |conn| {
    let key = crate::path_key::canonical_recorded_key(conn, &root.path.display().to_string());
    let tx = crate::write_tx(conn).unwrap();
    for i in 0..preceding + 100 {
        let path = format!("{key}/p{i:06}.mkv");
        tx.execute("INSERT INTO unidentified(library_id,root_id,path,path_key,confidence,unidentified_id) VALUES(?1,?2,?3,?3,'low',?3)",
            rusqlite::params![library.id,root.id,path]).unwrap();
    }
    for i in 0..unrelated {
        let path = format!("/outside-all-roots/p{i:06}.mkv");
        tx.execute("INSERT INTO unidentified(library_id,root_id,path,path_key,confidence,unidentified_id) VALUES(?1,?2,?3,?3,'low',?3)",
            rusqlite::params![library.id,root.id,path]).unwrap();
    }
    let cursor = if preceding == 0 {
        String::new()
    } else {
        format!("{key}/p{:06}.mkv", preceding - 1)
    };
    tx.execute("INSERT INTO pending_root_reassignments(kind,root_key,cursor_key,unidentified_cursor_key,unidentified_cursor_id)
        VALUES('movie',?1,?1,?2,?3)",rusqlite::params![key,cursor,cursor]).unwrap();
    tx.commit().unwrap();
    Ok(())
    }).unwrap();
    VM.with(|counter| counter.set(0));
    store
        .with_writer(crate::Db::Library, |conn| {
            conn.trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(observe));
            Ok(())
        })
        .unwrap();
    assert_eq!(store.drain_one_root_batch().unwrap(), Some(true));
    store
        .with_writer(crate::Db::Library, |conn| {
            conn.trace_v2(TraceEventCodes::empty(), None);
            Ok(())
        })
        .unwrap();
    let vm = VM.with(Cell::get);
    assert_eq!(
        store.list_unidentified(None).unwrap().len(),
        preceding + 100 + unrelated
    );
    assert_eq!(
        crate::test_support::pending_root_reassignments(&store).len(),
        1
    );
    vm
}

#[test]
fn candidate_window_seeks_past_preceding_and_unrelated_rows() {
    let small = window_work(100, 100);
    let large = window_work(5_000, 5_000);
    assert!(
        large <= small + 100,
        "candidate selection re-read preceding facts: {small} -> {large}"
    );
}

#[test]
#[ignore = "scale measurement; run with --release --ignored --nocapture"]
fn measure_unidentified_drain_work() {
    eprintln!("SQLite={}", rusqlite::version());
    for size in [5_000, 50_000, 100_000] {
        eprintln!(
            "preceding={size} unrelated={size} vm={}",
            window_work(size, size)
        );
    }
}
