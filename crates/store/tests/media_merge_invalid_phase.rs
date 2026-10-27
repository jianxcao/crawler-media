use rusqlite::{params, Connection};
mod merge_support;

#[test]
fn invalid_and_missing_merge_phase_are_rejected_without_moving_media() {
    for phase in [None, Some("planned"), Some("frozen"), Some("draining"), Some("applying"), Some("invalid")] {
        let f = merge_support::setup_pending_merge();
        let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
        db.execute("UPDATE media_operations SET phase_cursor = ?2 WHERE operation_id = ?1", params![f.op, phase]).unwrap();
        assert!(f.store.media_operation_status(&f.op).is_err(), "phase {phase:?}");
        assert!(f.store.advance_media_merge(&f.op, 7000).is_err(), "phase {phase:?}");
        assert!(f.store.get_media(f.loser).unwrap().is_some());
    }
}
