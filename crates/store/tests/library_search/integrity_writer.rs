//! Public Integrity progression and the work performed by its actual writer.
use super::*;
use std::cell::Cell;
use store::Db;

thread_local! {
    static WRITER_VM_STEPS: Cell<u64> = const { Cell::new(0) };
}

/// Count SQLite VDBE work on the real writer only. Reader connections never
/// receive this handler, so moving work to a reader changes this measurement.
fn writer_vm_steps(store: &Store, work: impl FnOnce()) -> u64 {
    struct Clear<'a>(&'a Store);
    impl Drop for Clear<'_> {
        fn drop(&mut self) {
            self.0
                .with_writer(Db::Library, |conn| {
                    conn.progress_handler(0, None::<fn() -> bool>);
                    Ok(())
                })
                .unwrap();
        }
    }
    store
        .with_writer(Db::Library, |conn| {
            conn.progress_handler(
                1,
                Some(|| {
                    WRITER_VM_STEPS.with(|steps| steps.set(steps.get() + 1));
                    false
                }),
            );
            Ok(())
        })
        .unwrap();
    let clear = Clear(store);
    WRITER_VM_STEPS.with(|steps| steps.set(0));
    work();
    let measured = WRITER_VM_STEPS.with(Cell::get);
    store
        .with_writer(Db::Library, |conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .unwrap();
    let still_installed = WRITER_VM_STEPS.with(Cell::get) > measured;
    drop(clear);
    assert!(measured > 0, "writer instrumentation must observe SQL work");
    assert!(
        still_installed,
        "advance must preserve the writer work counter"
    );
    measured
}

fn seek_header(store: &Store, operation: &str) {
    for step in 0..500 {
        let view = store.library_operation_view(operation).unwrap().unwrap();
        if view.cursor.as_ref().is_some_and(|cursor| {
            cursor.pointer("/current/phase").and_then(|p| p.as_str()) == Some("VerifyHeader")
        }) {
            return;
        }
        assert_ne!(view.state, "succeeded", "expected another Header: {view:?}");
        let status = store
            .advance_library_operation(operation, 2000 + step, 1)
            .unwrap();
        assert_ne!(status.state, "failed", "{status:?}");
    }
    panic!("finite fixture never reached its Header");
}

fn finish(store: &Store, operation: &str) {
    for step in 0..500 {
        let status = store
            .advance_library_operation(operation, 3000 + step, 1)
            .unwrap();
        if status.state == "succeeded" {
            return;
        }
        assert_ne!(status.state, "failed", "{status:?}");
    }
    panic!("finite fixture never finished");
}

fn header_work(unrelated: usize, header: usize) -> (u64, u64) {
    let f = Fixture::new();
    let target = f.library("target");
    let other = f.library("other");
    for n in 0..header {
        f.raw(target, &format!("Target Film {n}"));
    }
    for n in 0..unrelated {
        f.raw(
            other,
            &format!("Unrelated Film {n} with a long separate search document"),
        );
    }
    f.store
        .begin_library_integrity(target, true, "measure-header", 1000)
        .unwrap();
    for n in 0..header {
        seek_header(&f.store, "measure-header");
        if n + 1 < header {
            f.store
                .advance_library_operation("measure-header", 2500, 1)
                .unwrap();
        }
    }
    let (writer, total) = f.store.with_stats(StatsScope::Library, || {
        writer_vm_steps(&f.store, || {
            let status = f
                .store
                .advance_library_operation("measure-header", 2501, 1)
                .unwrap();
            assert_eq!(status.state, "applying");
        })
    });
    assert_eq!(
        f.store
            .library_lifecycle_state(&target.to_string())
            .unwrap(),
        "reindexing"
    );
    (writer, total.vm_steps)
}

#[test]
fn integrity_first_header_writer_work_is_independent_of_unrelated_fts() {
    let small = header_work(0, 1);
    let large = header_work(160, 1);
    eprintln!("first Header (writer progress,total VM): {small:?} -> {large:?}");
    assert!(
        large.0 <= small.0 + 2000,
        "a fixed Header must not scan unrelated FTS in its writer: {small:?} -> {large:?}"
    );
}

#[test]
fn integrity_later_header_total_work_is_independent_of_unrelated_fts() {
    let small = header_work(0, 2);
    let large = header_work(160, 2);
    eprintln!("later Header (writer progress,total VM): {small:?} -> {large:?}");
    assert!(
        large.1 <= small.1 + 2000,
        "later Headers must reuse operation evidence: {small:?} -> {large:?}"
    );
}

fn replace_postings(f: &Fixture, lib: LibraryId, media: MediaId, damage: &str) {
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    let (rowid, text): (i64, String) = db.query_row(
        "SELECT search_rowid,search_text FROM library_items WHERE library_id=?1 AND media_id=?2",
        params![lib.to_string(), media.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    db.execute("DELETE FROM library_search_fts WHERE rowid=?1", [rowid])
        .unwrap();
    let (text, people) = match damage {
        "column" => (String::new(), text),
        // These two texts have identical trigram multiplicities and boundaries;
        // only the offsets of their interior trigrams change.
        "offset" => (text.replacen("abcabdab", "abdabcab", 1), String::new()),
        "missing" => return,
        _ => panic!("unknown fixture damage"),
    };
    db.execute(
        "INSERT INTO library_search_fts(rowid,search_text,people) VALUES(?1,?2,?3)",
        params![rowid, text, people],
    )
    .unwrap();
}

#[test]
fn integrity_keeps_exact_fts_column_and_offset_detection() {
    for damage in ["column", "offset"] {
        let f = Fixture::new();
        let lib = f.library("target");
        let (media, _) = f.raw(lib, "abcabdab");
        replace_postings(&f, lib, media, damage);
        f.integrity(lib, false, "exact-check");
        let report = f.store.library_integrity_report("exact-check").unwrap();
        assert_eq!(report.query_projection_media, [media], "{damage}");
        assert_eq!(report.totals.query_projection_mismatches, 1, "{damage}");
        f.integrity(lib, true, "exact-repair");
        f.integrity(lib, false, "exact-verify");
        assert!(
            !f.store
                .library_integrity_report("exact-verify")
                .unwrap()
                .has_issues(),
            "{damage}"
        );
    }
}

#[test]
fn integrity_distinguishes_missing_fts_document_from_zero_tokens() {
    let f = Fixture::new();
    let lib = f.library("target");
    let (media, _) = f.raw(lib, "a");
    f.integrity(lib, false, "empty-tokens-clean");
    assert!(
        !f.store
            .library_integrity_report("empty-tokens-clean")
            .unwrap()
            .has_issues()
    );
    replace_postings(&f, lib, media, "missing");
    f.integrity(lib, false, "empty-tokens-missing");
    assert_eq!(
        f.store
            .library_integrity_report("empty-tokens-missing")
            .unwrap()
            .query_projection_media,
        [media]
    );
    f.integrity(lib, true, "empty-tokens-repair");
    f.integrity(lib, false, "empty-tokens-verify");
    assert!(
        !f.store
            .library_integrity_report("empty-tokens-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn integrity_restart_keeps_committed_counts_and_repairs_remaining_postings() {
    let f = Fixture::new();
    let lib = f.library("target");
    for _ in 0..2 {
        let (media, _) = f.raw(lib, "abcabdab");
        replace_postings(&f, lib, media, "offset");
    }
    f.store
        .begin_library_integrity(lib, true, "restart-repair", 1000)
        .unwrap();
    seek_header(&f.store, "restart-repair");
    f.store
        .advance_library_operation("restart-repair", 2500, 1)
        .unwrap();
    let before = f.store.library_integrity_report("restart-repair").unwrap();
    assert_eq!(before.totals.query_projection_mismatches, 1);
    let Fixture { dir, store } = f;
    drop(store);
    let f = Fixture {
        store: Store::open(dir.path().join("data")).unwrap(),
        dir,
    };
    assert_eq!(
        f.store.library_lifecycle_state(&lib.to_string()).unwrap(),
        "reindexing"
    );
    finish(&f.store, "restart-repair");
    assert_eq!(
        f.store
            .library_integrity_report("restart-repair")
            .unwrap()
            .totals
            .query_projection_mismatches,
        2
    );
    f.integrity(lib, false, "restart-verify");
    assert!(
        !f.store
            .library_integrity_report("restart-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn readonly_fts_vocab_remains_queryable_after_enabling_query_only() {
    use rusqlite::{Connection, OpenFlags};
    let f = Fixture::new();
    let lib = f.library("target");
    let (media, _) = f.raw(lib, "abcabdab");
    let conn = Connection::open_with_flags(
        f.dir.path().join("data/library.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let schema: i64 = conn
        .pragma_query_value(None, "schema_version", |r| r.get(0))
        .unwrap();
    conn.execute_batch("CREATE VIRTUAL TABLE temp.vocab_capability USING fts5vocab(main,library_search_fts,'instance')").unwrap();
    conn.pragma_update(None, "query_only", "ON").unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let actual: Vec<(String, String, i64)> = conn.prepare(
        "SELECT term,col,offset FROM temp.vocab_capability WHERE doc=(SELECT search_rowid FROM library_items WHERE library_id=?1 AND media_id=?2) ORDER BY term,col,offset",
    ).unwrap().query_map(params![lib.to_string(), media.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
    let expected = [
        ("ab\u{1f}", 6),
        ("abc", 0),
        ("abd", 3),
        ("bca", 1),
        ("bda", 4),
        ("cab", 2),
        ("dab", 5),
    ]
    .into_iter()
    .map(|(term, offset)| (term.to_string(), "search_text".to_string(), offset))
    .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert!(
        conn.execute_batch("CREATE TEMP TABLE forbidden(value INTEGER)")
            .is_err()
    );
    assert!(conn.execute("DELETE FROM library_search_fts", []).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "schema_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        schema
    );
}
