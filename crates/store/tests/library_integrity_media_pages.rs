use domain::MediaKind;
mod library_integrity_support;
use library_integrity_support::*;
use rusqlite::params;
use store::{Store, StoreError, stats::StatsScope};

#[test]
fn one_long_media_yields_and_resumes_inside_check_and_repair() {
    for repair in [false, true] {
        let fixture = fixture(1_000);
        fixture
            .store
            .begin_library_integrity(fixture.library, repair, "long", 0)
            .unwrap();
        let initial = fixture
            .store
            .library_operation_view("long")
            .unwrap()
            .unwrap();
        let status = fixture
            .store
            .advance_library_operation("long", 1, 1)
            .unwrap();
        assert_eq!(
            status.state, "applying",
            "one Media is larger than one fact page"
        );
        let progress = fixture
            .store
            .library_operation_view("long")
            .unwrap()
            .unwrap();
        assert_ne!(progress.cursor, initial.cursor);
        assert!(matches!(
            fixture.store.ensure_library_query_ready(fixture.library),
            Err(StoreError::QueryUnavailable { .. })
        ));
        let unrelated = fixture
            .store
            .create_library(
                MediaKind::Movie,
                "Still writable",
                &["/integrity-unrelated"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        assert!(fixture.store.get_library(&unrelated.id).unwrap().is_some());
        drop(fixture.store);

        let store = Store::open(fixture.tmp.path()).unwrap();
        assert_eq!(
            store
                .library_operation_view("long")
                .unwrap()
                .unwrap()
                .cursor,
            progress.cursor
        );
        finish(&store, "long");
        let report = store.library_integrity_report("long").unwrap();
        assert_eq!(report.totals.missing_items, 1);
        assert_eq!(report.totals.missing_user_items, 1);
        if repair {
            let summary = store
                .get_user_library_summary(fixture.user, fixture.library, fixture.media)
                .unwrap()
                .unwrap();
            assert_eq!(summary.owned_episode_count, 1_000);
            assert_eq!(summary.completed_episode_count, 0);
            assert!(summary.favorite);
            assert_eq!(
                (summary.next_season, summary.next_episode),
                (Some(1), Some(1))
            );
            store
                .begin_library_integrity(fixture.library, false, "verify", 100_000)
                .unwrap();
            finish(&store, "verify");
            assert!(
                !store
                    .library_integrity_report("verify")
                    .unwrap()
                    .has_issues()
            );
        }
    }
}

#[test]
fn first_fact_page_work_does_not_expand_with_the_whole_media() {
    let measure = |episodes| {
        let fixture = fixture(episodes);
        fixture
            .store
            .begin_library_integrity(fixture.library, false, "work", 0)
            .unwrap();
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture.store.advance_library_operation("work", 1, 1)
        });
        result.unwrap();
        work
    };
    let small = measure(1_000);
    let large = measure(10_000);
    assert!(small.sql_rows > 0, "measure real database work: {small:?}");
    assert!(
        large.sql_rows <= small.sql_rows + 200,
        "one fact page decoded the whole Media: {small:?} -> {large:?}"
    );
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "one fact page scanned the whole Media: {small:?} -> {large:?}"
    );
}

#[test]
fn changed_frozen_revision_cannot_publish_old_integrity_progress() {
    let fixture = fixture(1_000);
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "revision", 0)
        .unwrap();
    let first = fixture
        .store
        .advance_library_operation("revision", 1, 1)
        .unwrap();
    assert_eq!(first.state, "applying");
    fixture
        .store
        .advance_library_operation("revision", 2, 1)
        .unwrap();
    let before = fixture
        .store
        .library_operation_view("revision")
        .unwrap()
        .unwrap();
    let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE libraries SET config_revision=config_revision+1 WHERE library_id=?1",
        [fixture.library.to_string()],
    )
    .unwrap();
    assert!(matches!(
        fixture.store.advance_library_operation("revision", 3, 1),
        Err(StoreError::Protected(_))
    ));
    let after = fixture
        .store
        .library_operation_view("revision")
        .unwrap()
        .unwrap();
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.state, "failed");
    assert!(
        after
            .error
            .as_deref()
            .is_some_and(|s| s.contains("revision"))
    );
    let scratch = || {
        db.query_row("SELECT count(*) FROM library_integrity_rows", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let staged = scratch();
    assert!(staged > 0);
    fixture
        .store
        .prune_terminal_operations(i64::MAX, 100, false)
        .unwrap();
    assert_eq!(
        scratch(),
        staged,
        "failed owner scratch must remain resumable"
    );
    assert!(
        fixture
            .store
            .library_operation_view("revision")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture
            .store
            .library_lifecycle_state(&fixture.library.to_string())
            .unwrap(),
        "reindexing"
    );
}

#[test]
fn final_publication_failure_retains_owner_and_cursor_then_retries() {
    let fixture = fixture(1);
    let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_integrity_counts BEFORE INSERT ON library_query_counts
        BEGIN SELECT RAISE(FAIL,'injected count publication failure'); END;",
    )
    .unwrap();
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "failure", 0)
        .unwrap();
    let mut last = None;
    for step in 1..200 {
        last = fixture
            .store
            .library_operation_view("failure")
            .unwrap()
            .unwrap()
            .cursor;
        if fixture
            .store
            .advance_library_operation("failure", step, 1)
            .is_err()
        {
            break;
        }
    }
    let failed = fixture
        .store
        .library_operation_view("failure")
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.cursor, last);
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|s| s.contains("injected"))
    );
    assert_eq!(
        fixture
            .store
            .active_library_operation(&fixture.library.to_string())
            .unwrap()
            .as_deref(),
        Some("failure")
    );
    db.execute_batch("DROP TRIGGER reject_integrity_counts")
        .unwrap();
    finish(&fixture.store, "failure");
    let done = fixture
        .store
        .library_operation_view("failure")
        .unwrap()
        .unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.error, None);
}

#[test]
fn pending_file_prefix_does_not_expand_one_fact_page() {
    let measure = |count| {
        let fixture = fixture(1);
        add_pending_files(&fixture, count);
        fixture
            .store
            .begin_library_integrity(fixture.library, false, "pending", 0)
            .unwrap();
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture.store.advance_library_operation("pending", 1, 1)
        });
        result.unwrap();
        work
    };
    let small = measure(1_000);
    let large = measure(10_000);
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "pending prefix scanned the whole Media: {small:?} -> {large:?}"
    );
}

#[test]
fn one_user_with_many_facts_does_not_expand_user_selection() {
    let measure = |count| {
        let fixture = fixture(1);
        let mut db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
        let tx = db.transaction().unwrap();
        for episode in 1..=count {
            tx.execute(
                "INSERT INTO playback_units(user_id,media_id,season,episode)
                VALUES(?1,?2,1,?3)",
                params![fixture.user.to_string(), fixture.media.to_string(), episode],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        fixture
            .store
            .begin_library_integrity(fixture.library, false, "users", 0)
            .unwrap();
        for step in 1..100 {
            let view = fixture
                .store
                .library_operation_view("users")
                .unwrap()
                .unwrap();
            if view
                .cursor
                .as_ref()
                .is_some_and(|c| c["current"]["phase"] == "Users")
            {
                break;
            }
            fixture
                .store
                .advance_library_operation("users", step, 1)
                .unwrap();
        }
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture.store.advance_library_operation("users", 100, 1)
        });
        result.unwrap();
        work
    };
    let small = measure(1_000);
    let large = measure(10_000);
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "one User candidate scanned every fact: {small:?} -> {large:?}"
    );
}

#[test]
fn continue_reverse_page_seeks_before_limiting_many_users() {
    let measure = |count| {
        let fixture = fixture(1);
        many_derived_users(&fixture, count);
        fixture
            .store
            .begin_library_integrity(fixture.library, false, "reverse", 0)
            .unwrap();
        reach_reverse(&fixture, "reverse", 2, count * 2 + 500);
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture
                .store
                .advance_library_operation("reverse", 100_000, 1)
        });
        result.unwrap();
        work
    };
    let small = measure(300);
    let large = measure(3_000);
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "continue page sorted every User: {small:?} -> {large:?}"
    );
    assert_eq!(
        large.sort_operations, 0,
        "continue page must seek in cursor order"
    );
}

#[test]
fn deleting_one_unit_drains_only_one_page_of_user_children() {
    let measure = |count| {
        let fixture = fixture(1);
        many_derived_users(&fixture, count);
        let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
        db.execute(
            "UPDATE library_units SET available_file_count=0,preferred_file_id=NULL",
            [],
        )
        .unwrap();
        db.execute("DELETE FROM library_file_units", []).unwrap();
        fixture
            .store
            .begin_library_integrity(fixture.library, true, "children", 0)
            .unwrap();
        reach_reverse(&fixture, "children", 0, count * 2 + 500);
        let remaining = || {
            db.query_row(
                "SELECT count(*) FROM user_library_continue_units",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        let before = remaining();
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture
                .store
                .advance_library_operation("children", 100_000, 1)
        });
        result.unwrap();
        assert_eq!(
            before - remaining(),
            100,
            "one unit must not cascade every User child"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM library_units", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        work
    };
    let small = measure(300);
    let large = measure(3_000);
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "one unit cleanup scanned every User: {small:?} -> {large:?}"
    );
    assert_eq!(large.sort_operations, 0);
}

#[test]
fn media_with_only_orphan_units_still_has_frozen_identity() {
    let fixture = fixture(1);
    let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db.execute(
        "INSERT INTO library_units(library_id,media_id,season,episode,file_count,
        available_file_count,unit_revision) VALUES(?1,?2,1,1,1,0,1)",
        params![fixture.library.to_string(), fixture.media.to_string()],
    )
    .unwrap();
    db.execute("DELETE FROM library_files", []).unwrap();
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "orphan-owner", 0)
        .unwrap();
    let mut media = fixture.store.get_media(fixture.media).unwrap().unwrap();
    media.title = "Changed while orphan is staged".into();
    assert!(matches!(
        fixture.store.update_media(&media),
        Err(StoreError::Protected(_))
    ));
    assert!(matches!(
        fixture.store.mutate_playback(
            fixture.media,
            &store::playback::PlaybackMutation::marks(fixture.user, -1, -1, Some(true), None),
            1
        ),
        Err(StoreError::Protected(_))
    ));
}

#[test]
fn many_versions_of_one_unit_and_resolution_terms_reduce_across_pages() {
    let fixture = fixture(1);
    add_pending_files(&fixture, 1_000);
    let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE library_files SET publication_state='published',resolution=file_id",
        [],
    )
    .unwrap();
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "versions", 0)
        .unwrap();
    let mut cleanup_seen = false;
    for step in 1..10_000 {
        let before = fixture
            .store
            .library_operation_view("versions")
            .unwrap()
            .unwrap();
        let count = || {
            db.query_row(
                "SELECT count(*) FROM library_integrity_rows WHERE operation_id='versions'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        let rows = count();
        let status = fixture
            .store
            .advance_library_operation("versions", step, 1)
            .unwrap();
        if before
            .cursor
            .as_ref()
            .is_some_and(|c| c["current"]["phase"] == "Cleanup")
            && rows > 0
        {
            cleanup_seen = true;
            assert_eq!(rows - count(), rows.min(100));
            fixture
                .store
                .prune_terminal_operations(i64::MAX, 100, false)
                .unwrap();
            assert!(
                fixture
                    .store
                    .library_operation_view("versions")
                    .unwrap()
                    .is_some()
            );
        }
        if status.state == "succeeded" {
            break;
        }
    }
    assert!(cleanup_seen);
    assert_eq!(
        db.query_row("SELECT count(*) FROM library_integrity_rows", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT file_count FROM library_units", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1_001
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM library_item_terms WHERE term_kind='resolution'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1_001
    );
    let summary = fixture
        .store
        .get_user_library_summary(fixture.user, fixture.library, fixture.media)
        .unwrap()
        .unwrap();
    assert_eq!(summary.owned_episode_count, 1);
    fixture
        .store
        .begin_library_integrity(fixture.library, false, "versions-verify", 20_000)
        .unwrap();
    finish(&fixture.store, "versions-verify");
    assert!(
        !fixture
            .store
            .library_integrity_report("versions-verify")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn header_publication_uses_the_paged_first_available_unit() {
    let measure = |episodes| {
        let fixture = fixture(episodes);
        let db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
        db.execute("UPDATE library_files SET presence_state='missing'", [])
            .unwrap();
        fixture
            .store
            .begin_library_integrity(fixture.library, true, "header", 0)
            .unwrap();
        let mut reached = false;
        for step in 0..2_000 {
            let before = fixture
                .store
                .library_operation_view("header")
                .unwrap()
                .unwrap();
            if before
                .cursor
                .as_ref()
                .is_some_and(|c| c["current"]["phase"] == "VerifyHeader")
            {
                reached = true;
                break;
            }
            fixture
                .store
                .advance_library_operation("header", step, 1)
                .unwrap();
        }
        assert!(reached);
        let (result, work) = fixture.store.with_stats(StatsScope::Library, || {
            fixture.store.advance_library_operation("header", 10_000, 1)
        });
        result.unwrap();
        work
    };
    let small = measure(1_000);
    let large = measure(10_000);
    assert!(
        large.vm_steps <= small.vm_steps * 2 + 2_000,
        "header rescanned all unavailable units: {small:?} -> {large:?}"
    );
}
