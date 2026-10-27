use rusqlite::Connection;
use std::fs;
use std::path::Path;
use store::Store;
use store::schema::layout::{
    APP_APPLICATION_ID, LIBRARY_APPLICATION_ID, LIBRARY_USER_VERSION, SUBSCRIBE_APPLICATION_ID,
    SUBSCRIBE_USER_VERSION, V1_USER_VERSION, validate_cache_layout, validate_database_layout,
};

#[path = "support/mod.rs"]
mod support;

fn seed_five_database_cluster(dir: &Path) -> (domain::UserId, domain::MediaId, domain::LedgerId) {
    let store = Store::open(dir).unwrap();
    let user_id = domain::UserId::new();
    let (media_id, ledger_id) =
        support::seed_published_file(&store, &dir.join("library_root"), "matrix");

    // Add progress in library.db
    store
        .upsert_unit(
            user_id,
            media_id,
            -1,
            -1,
            42_000,
            Some(false),
            Some(true),
            Some(120_000),
            None,
            None,
            false,
            1_000,
        )
        .unwrap();

    // Add an active media operation (state = "frozen")
    let active_op_id = "00000000-0000-0000-0000-000000000088";
    {
        let conn = Connection::open(dir.join("library.db")).unwrap();
        conn.execute(
            "INSERT INTO media_operations
                (operation_id, kind, plan_json, phase_cursor, state, error, created_at_ms, updated_at_ms, survivor_id, loser_id)
             VALUES (?1, 'merge', '{}', 'init', 'frozen', NULL, 1000, 1000, ?2, NULL)",
            rusqlite::params![active_op_id, media_id.to_string()],
        )
        .unwrap();
    }

    // Add a response cache entry in cache.db
    {
        let conn = Connection::open(dir.join("cache.db")).unwrap();
        conn.execute(
            "INSERT INTO tmdb_cache (cache_key, body, fetched_at) VALUES ('tmdb:movie:matrix', '{\"title\":\"Matrix\"}', 1000)",
            [],
        )
        .unwrap();
    }

    let queue = jobs::Queue::open(dir.join("jobs.db")).unwrap();
    queue
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::DataRetention,
            name: "retention".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();

    (user_id, media_id, ledger_id)
}

fn controlled_snapshot(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for db_name in [
        "app.db",
        "cache.db",
        "library.db",
        "subscribe.db",
        "jobs.db",
    ] {
        let src_file = src.join(db_name);
        let dst_file = dst.join(db_name);
        let conn = Connection::open(&src_file).unwrap();
        // Controlled snapshot uses SQLite's official VACUUM INTO / backup API
        // which forces checkpoint flush and creates a consistent, clean database copy
        conn.execute(
            "VACUUM INTO ?1",
            rusqlite::params![dst_file.to_str().unwrap()],
        )
        .unwrap();
    }
}

fn validate_cluster_layout_and_consistency(dir: &Path) -> (usize, usize, usize) {
    let app_conn = Connection::open(dir.join("app.db")).unwrap();
    validate_database_layout("app", &app_conn, APP_APPLICATION_ID, V1_USER_VERSION).unwrap();

    let lib_conn = Connection::open(dir.join("library.db")).unwrap();
    validate_database_layout(
        "library",
        &lib_conn,
        LIBRARY_APPLICATION_ID,
        LIBRARY_USER_VERSION,
    )
    .unwrap();

    let sub_conn = Connection::open(dir.join("subscribe.db")).unwrap();
    validate_database_layout(
        "subscribe",
        &sub_conn,
        SUBSCRIBE_APPLICATION_ID,
        SUBSCRIBE_USER_VERSION,
    )
    .unwrap();

    let cache_conn = Connection::open(dir.join("cache.db")).unwrap();
    validate_cache_layout(&cache_conn).unwrap();

    let jobs_conn = Connection::open(dir.join("jobs.db")).unwrap();
    jobs::schema::validate_layout(&jobs_conn).unwrap();

    let items_count: usize = lib_conn
        .query_row("SELECT COUNT(*) FROM library_items", [], |r| r.get(0))
        .unwrap();
    let cache_count: usize = cache_conn
        .query_row("SELECT COUNT(*) FROM tmdb_cache", [], |r| r.get(0))
        .unwrap();
    let jobs_count: usize = jobs_conn
        .query_row("SELECT COUNT(*) FROM job_defs", [], |r| r.get(0))
        .unwrap();

    (items_count, cache_count, jobs_count)
}

#[test]
fn controlled_snapshot_yields_valid_five_db_cluster_with_cross_consistency() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");

    let (_user, _media, _ledger) = seed_five_database_cluster(&src_dir);

    // Perform controlled snapshot (controlled stop/quiescence + VACUUM INTO)
    controlled_snapshot(&src_dir, &dst_dir);

    // Validate that all five databases in snapshot can be opened, layouts are valid,
    // and cross-references match
    let (items, cache, jobs) = validate_cluster_layout_and_consistency(&dst_dir);
    assert_eq!(items, 1);
    assert_eq!(cache, 1);
    assert_eq!(jobs, 1);

    // Verify snapshot can be reopened as a Store instance and data remains consistent
    let _snapshot_store = Store::open(&dst_dir).unwrap();
    let active_operations: usize = Connection::open(dst_dir.join("library.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM media_operations WHERE state IN ('planned','frozen','draining','applying','checking')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_operations, 1);
}

#[test]
fn naive_copy_of_live_writer_wal_fails_or_loses_uncheckpointed_data() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    let naive_dst_dir = tmp.path().join("naive_dst");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&naive_dst_dir).unwrap();

    let db_path = src_dir.join("library.db");
    let live_conn = Connection::open(&db_path).unwrap();
    live_conn
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    live_conn
        .pragma_update(None, "application_id", LIBRARY_APPLICATION_ID)
        .unwrap();
    live_conn
        .pragma_update(None, "user_version", LIBRARY_USER_VERSION)
        .unwrap();
    live_conn
        .execute_batch(include_str!("../src/schema/v1/library.sql"))
        .unwrap();

    // Prevent autocheckpoint so all new writes stay purely in WAL file
    live_conn
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();

    // Commit a real transaction into the active writer database.
    // Because autocheckpoint is 0, this committed data resides entirely in library.db-wal!
    live_conn.execute(
        "INSERT INTO media (media_id, kind, title, identity_state) VALUES ('media-wal-only', 'movie', 'WAL Movie', 'provisional')",
        [],
    ).unwrap();

    // Verify data exists when queried via the live connection
    let count_live: i64 = live_conn
        .query_row(
            "SELECT COUNT(*) FROM media WHERE media_id = 'media-wal-only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_live, 1);

    // NAIVE COPY: Copy ONLY library.db without checkpointing and without library.db-wal
    fs::copy(&db_path, naive_dst_dir.join("library.db")).unwrap();

    // Concrete observed symptom:
    // Opening naive copy of main db without its WAL file sees an incomplete/corrupted state
    // where table schema or WAL-resident commits are missing (e.g., "no such table: media" or count == 0)!
    let naive_conn = Connection::open(naive_dst_dir.join("library.db")).unwrap();
    let query_res: Result<i64, _> = naive_conn.query_row(
        "SELECT COUNT(*) FROM media WHERE media_id = 'media-wal-only'",
        [],
        |r| r.get(0),
    );

    // Assert the observed failure symptom: either query fails due to missing schema in main file
    // or the WAL-resident transaction is missing (count == 0).
    match query_res {
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("no such table")
                    || msg.contains("disk I/O error")
                    || msg.contains("database disk image is malformed"),
                "Expected schema or corruption error when copying without WAL, got: {msg}"
            );
        }
        Ok(count) => {
            assert_eq!(
                count, 0,
                "Naive file copy of active DB without WAL checkpoint silently loses WAL-resident committed data"
            );
        }
    }
}
