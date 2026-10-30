//! Task 6: production deletion plans, cascade behavior and cross-DB recovery.
#[path = "library_removal_plans/support.rs"]
mod support;
use support::*;

#[test]
fn orphan_purge_and_probe_unit_retirement_use_indexes() {
    let fixture = Fixture::new();
    let connection = fixture.connection("library.db");
    let queries = production_removal_sql();
    assert!(!queries.is_empty());
    let mut scans = Vec::new();
    for sql in queries {
        let plans = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(["missing-fixture-id"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!plans.is_empty(), "{sql}");
        for plan in plans {
            if plan
                .to_ascii_uppercase()
                .split_whitespace()
                .any(|word| word == "SCAN")
            {
                scans.push(format!("{sql}: {plan}"));
            }
        }
    }
    assert!(
        scans.is_empty(),
        "removal must seek indexes, including aliased nodes:\n{}",
        scans.join("\n")
    );
}

#[test]
fn orphan_purge_still_removes_projection_rows() {
    let fixture = Fixture::new();
    let connection = fixture.connection("library.db");
    let mut records = Vec::new();
    let user = domain::UserId::new();
    for name in ["Removed", "Independent"] {
        let library = fixture.library(name);
        let media = fixture.media(name);
        let path = library.root_paths[0].join(format!("{name}.mkv"));
        std::fs::write(&path, b"original disk bytes").unwrap();
        // Populate terms and people through the real projection reducer so its
        // materialized counters remain consistent when removal subtracts them.
        std::fs::write(
            path.with_extension("nfo"),
            format!("<movie><title>{name}</title><genre>Drama</genre><actor><name>Fixture Person</name><role>Lead</role></actor></movie>"),
        )
        .unwrap();
        let file = store::test_support::publish_fixture_file(
            &fixture.store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        fixture
            .store
            .upsert_unit(
                user,
                media.id,
                -1,
                -1,
                1000,
                Some(false),
                Some(true),
                Some(10000),
                None,
                None,
                false,
                1000,
            )
            .unwrap();
        assert!(counts(&connection, media.id).iter().all(|count| *count > 0));
        records.push((library, media, file));
    }
    let independent = counts(&connection, records[1].1.id);
    fixture.remove(&records[0].0);
    assert!(fixture.store.get_media(records[0].1.id).unwrap().is_none());
    assert_eq!(counts(&connection, records[0].1.id), vec![0; 4]);
    assert!(
        fixture
            .store
            .unit_state(user, records[0].1.id, -1, -1)
            .unwrap()
            .is_none()
    );
    assert_eq!(counts(&connection, records[1].1.id), independent);
    assert_eq!(
        std::fs::read(&records[0].2.path).unwrap(),
        b"original disk bytes"
    );
    assert!(fixture.store.get_media(records[1].1.id).unwrap().is_some());
    let preserved = fixture
        .store
        .unit_state(user, records[1].1.id, -1, -1)
        .unwrap()
        .unwrap();
    assert_eq!(preserved.position_ms, 1000);
    assert!(preserved.favorite);
    let violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn succeeded_removal_retries_detachment_after_subscribe_commit_failure() {
    let fixture = Fixture::new();
    let library = fixture.library("Detachment recovery");
    let subscribe = fixture.subscribe(&library);
    let connection = fixture.connection("subscribe.db");
    connection.execute_batch("CREATE TRIGGER fail_library_detach BEFORE UPDATE OF library_id ON subscribes WHEN OLD.library_id IS NOT NULL AND NEW.library_id IS NULL BEGIN SELECT RAISE(ABORT,'injected subscribe detachment failure'); END;").unwrap();
    let operation = fixture.store.begin_library_removal(&library.id).unwrap();
    fixture
        .store
        .set_removal_state(&operation.operation_id, "cleaning", None)
        .unwrap();
    let failed = fixture.store.clean_library_removal(&operation.operation_id);
    assert!(
        failed.is_err(),
        "library success cannot hide failed subscribe detachment: {failed:?}"
    );
    assert!(
        fixture.store.get_library(&library.id).unwrap().is_none(),
        "library transaction must commit before subscribe detachment"
    );
    assert_eq!(
        fixture
            .store
            .library_removal(&operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        "succeeded"
    );
    assert_eq!(
        fixture
            .store
            .get_subscribe(subscribe)
            .unwrap()
            .unwrap()
            .library_id,
        Some(library.id.parse().unwrap())
    );
    connection
        .execute_batch("DROP TRIGGER fail_library_detach")
        .unwrap();
    let data = fixture.temp.path().join("data");
    drop(fixture.store);
    let reopened = store::Store::open(data).unwrap();
    let recovered = reopened
        .clean_library_removal(&operation.operation_id)
        .unwrap();
    assert_eq!(recovered.state, "succeeded");
    assert_eq!(
        reopened
            .get_subscribe(subscribe)
            .unwrap()
            .unwrap()
            .library_id,
        None
    );
    assert_eq!(
        reopened
            .clean_library_removal(&operation.operation_id)
            .unwrap(),
        recovered
    );
}
