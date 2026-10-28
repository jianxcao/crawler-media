use rusqlite::{Connection, StatementStatus, params};

fn work(history: usize, pending: usize) -> (i32, i32, i32, Vec<String>) {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../src/schema/v1/library.sql"))
        .unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for i in 0..history + pending {
        let state = if i < history { "succeeded" } else { "planned" };
        tx.execute("INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,plan_json,created_at_ms,updated_at_ms,resource_key) VALUES (?1,'lib','scrape',?2,1,'{}',0,?3,?1)", params![format!("op-{i:05}"), state, i as i64]).unwrap();
    }
    tx.commit().unwrap();
    db.execute_batch("ANALYZE").unwrap();
    let mut stmt = db.prepare("SELECT operation_id, plan_json FROM library_operations WHERE kind = ?1 AND state NOT IN ('succeeded', 'failed') ORDER BY updated_at_ms, operation_id").unwrap();
    let rows = stmt
        .query_map(["scrape"], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (
        stmt.get_status(StatementStatus::VmStep),
        stmt.get_status(StatementStatus::FullscanStep),
        stmt.get_status(StatementStatus::Sort),
        rows,
    )
}

#[test]
fn pending_query_work_is_independent_of_terminal_history() {
    for pending in [0, 1, 20] {
        let small = work(1000, pending);
        let large = work(20000, pending);
        assert_eq!(
            large.3,
            (20000..20000 + pending)
                .map(|i| format!("op-{i:05}"))
                .collect::<Vec<_>>()
        );
        assert!(
            large.1 <= pending.saturating_sub(1) as i32,
            "fullscan steps: {}",
            large.1
        );
        assert_eq!(large.2, 0);
        assert!(
            large.0 <= small.0 * 2,
            "small={} large={}",
            small.0,
            large.0
        );
    }
}
