mod playback_event_support;

use playback_event_support::*;
use rusqlite::{Connection, params};
use store::stats::StatsScope;

type WorkMetrics = (u64, u64, u64, u64);

fn work(history: usize) -> (WorkMetrics, WorkMetrics) {
    let (tmp, mut fixture) = seed();
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for i in 0..history {
        tx.execute(
            "INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,plan_json,created_at_ms,updated_at_ms,resource_key) VALUES (?1,'unrelated','scrape','succeeded',0,'{}',0,?2,?1)",
            params![format!("history-{i}"), i as i64],
        ).unwrap();
    }
    tx.commit().unwrap();
    db.execute_batch("ANALYZE").unwrap();
    drop(db);
    drop(fixture.store);
    fixture.store = Store::open(tmp.path()).unwrap();

    let media = fixture.store.get_media(fixture.media_id).unwrap().unwrap();
    let (result, write) = fixture
        .store
        .with_stats(StatsScope::Library, || fixture.store.update_media(&media));
    result.unwrap();
    let item = episode(&fixture, 1);
    let (session, generation) = issue(&fixture, &item, &fixture.file);
    let progress = event(
        &fixture,
        item,
        (&session, generation),
        &fixture.file,
        500,
        PlaybackEventType::Progress,
    );
    let (result, playback) = fixture.store.with_stats(StatsScope::Library, || {
        fixture.store.commit_playback_event(&progress, 2_000)
    });
    assert_eq!(result.unwrap(), EventOutcome::Applied);
    let counts = |s: store::library_query::items::QueryWorkStats| {
        (
            s.statements,
            s.vm_steps,
            s.fullscan_steps,
            s.sort_operations,
        )
    };
    (counts(write), counts(playback))
}

#[test]
fn unrelated_terminal_history_does_not_expand_write_barrier_work() {
    let small = work(1_000);
    let large = work(20_000);
    println!("write/progress work at 1k={small:?}, 20k={large:?} (SQL, VM, fullscan, sort)");
    for (name, before, after) in [
        ("update_media", small.0, large.0),
        ("progress", small.1, large.1),
    ] {
        assert_eq!(after.0, before.0, "{name} SQL count");
        assert!(
            after.1 <= before.1 + 100,
            "{name} VM steps: {before:?} -> {after:?}"
        );
        assert_eq!(
            after.2, before.2,
            "{name} fullscan: {before:?} -> {after:?}"
        );
        assert_eq!(after.3, before.3, "{name} sort count");
    }
}

#[test]
fn active_and_failed_integrity_freeze_only_the_owning_library() {
    let (tmp, fixture) = seed();
    let item = episode(&fixture, 1);
    let (session, generation) = issue(&fixture, &item, &fixture.file);
    let progress = event(
        &fixture,
        item,
        (&session, generation),
        &fixture.file,
        500,
        PlaybackEventType::Progress,
    );
    let media = fixture.store.get_media(fixture.media_id).unwrap().unwrap();
    let mut unrelated = media.clone();
    unrelated.id = MediaId::new();
    unrelated.tmdb_id = None;
    fixture.store.insert_media(&unrelated).unwrap();
    fixture
        .store
        .begin_library_integrity(fixture.library_id.parse().unwrap(), true, "integrity", 0)
        .unwrap();
    for state in ["planned", "failed"] {
        if state == "failed" {
            Connection::open(tmp.path().join("library.db"))
                .unwrap()
                .execute(
                    "UPDATE library_operations SET state='failed' WHERE operation_id='integrity'",
                    [],
                )
                .unwrap();
        }
        assert!(
            matches!(
                fixture.store.update_media(&media),
                Err(store::StoreError::Protected(_))
            ),
            "{state}"
        );
        assert!(
            matches!(
                fixture.store.commit_playback_event(&progress, 2_000),
                Err(store::StoreError::Protected(_))
            ),
            "{state}"
        );
        fixture.store.update_media(&unrelated).unwrap();
    }
}

#[test]
fn previous_library_layout_is_rejected_without_modifying_any_database() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("DROP INDEX library_integrity_owner; PRAGMA user_version=7;")
        .unwrap();
    drop(db);
    let hashes = || {
        ["app", "library", "subscribe", "cache"].map(|name| {
            Sha256::digest(std::fs::read(tmp.path().join(format!("{name}.db"))).unwrap())
        })
    };
    let before = hashes();
    assert!(matches!(
        Store::open(tmp.path()),
        Err(store::StoreError::LayoutMismatch(_))
    ));
    assert_eq!(hashes(), before);
}

fn index_write_cost(index: bool) -> (i64, i64, i64) {
    use rusqlite::StatementStatus;
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../src/schema/v1/library.sql"))
        .unwrap();
    if !index {
        db.execute_batch("DROP INDEX library_integrity_owner")
            .unwrap();
    }
    let tx = db.unchecked_transaction().unwrap();
    let mut insert_vm = 0;
    {
        let mut statement = tx.prepare("INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,plan_json,created_at_ms,updated_at_ms) VALUES (?1,'library','integrity','planned',0,'{}',0,0)").unwrap();
        for i in 0..1_000 {
            statement.execute([format!("integrity-{i:04}")]).unwrap();
            insert_vm += i64::from(statement.reset_status(StatementStatus::VmStep));
        }
    }
    tx.commit().unwrap();
    let bytes = if index {
        db.query_row(
            "SELECT coalesce(sum(pgsize),0) FROM dbstat WHERE name='library_integrity_owner'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    } else {
        0
    };
    let mut complete = db
        .prepare("UPDATE library_operations SET state='succeeded' WHERE operation_id=?1")
        .unwrap();
    let mut complete_vm = 0;
    for i in 0..1_000 {
        complete.execute([format!("integrity-{i:04}")]).unwrap();
        complete_vm += i64::from(complete.reset_status(StatementStatus::VmStep));
    }
    (insert_vm, complete_vm, bytes)
}

#[test]
fn integrity_index_write_and_page_cost_is_measurable() {
    let without = index_write_cost(false);
    let with = index_write_cost(true);
    println!(
        "1,000 Integrity inserts/completions: without={without:?}, with={with:?} (insert VM, complete VM, index bytes)"
    );
    assert!(with.0 > without.0 && with.1 > without.1 && with.2 > 0);
}
