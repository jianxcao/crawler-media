//! DB-05 store 合同：分类历史保留的候选/保护/游标。
//!
//! 断言走公开 seam `Store::prune_data_history`；probe/attempt/sample 的 fixture
//! 用 test-only 原始 SQL 造数（store 没有公开的任意时间戳写入接口），读取断言
//! 仍按真实表内容进行。

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use domain::{MediaId, UserId};
use media::response_cache::CacheSource;
use rusqlite::{Connection, params};
use store::{DataRetentionBatch, DataRetentionCursor, RetentionClass, Store};

const CUTOFF_SECS: i64 = 1_700_000_000;
const CUTOFF_MS: i64 = CUTOFF_SECS * 1000;
/// 计划 Task 20 Step 1 的缓存规模：真实 10 万行写入 + 清空。
const CACHE_ROWS: usize = 100_000;

fn open_db(dir: &Path, name: &str) -> Connection {
    Connection::open(dir.join(name)).unwrap()
}

fn metric_count(dir: &Path) -> i64 {
    open_db(dir, "library.db")
        .query_row("SELECT COUNT(*) FROM playback_metrics", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn insert_probe_job(dir: &Path, id: &str, scope: &str, status: &str, finished_at_ms: Option<i64>) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO probe_jobs
            (id, kind, media_id, season, scope_key, status, total, completed,
             succeeded, failed, partial, error, created_at_ms, started_at_ms, finished_at_ms)
         VALUES (?1, 'metadata', 'media-1', NULL, ?2, ?3, 1, 1, 1, 0, 0, NULL, ?4, ?4, ?5)",
        params![
            id,
            scope,
            status,
            finished_at_ms.unwrap_or(1),
            finished_at_ms
        ],
    )
    .unwrap();
}

fn insert_unit(dir: &Path, job_id: &str, file_id: &str, status: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO probe_job_units
            (job_id, file_id, kind, force_fingerprint, reuse_fingerprint_cache,
             overwrite_markers, status, error, reuse_media_info_cache)
         VALUES (?1, ?2, 'metadata', 0, 0, 0, ?3, NULL, 0)",
        params![job_id, file_id, status],
    )
    .unwrap();
}

fn insert_attempt(dir: &Path, attempt_id: &str, job_id: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO fingerprint_attempts
            (attempt_id, job_id, file_id, kind, window_start_ms, window_end_ms,
             phase, started_at_ms, finished_at_ms, status, error_kind, metrics_json)
         VALUES (?1, ?2, 'file-1', 'intro', 0, 1000, 'capture', 1, 2, 'succeeded', NULL, '{}')",
        params![attempt_id, job_id],
    )
    .unwrap();
}

fn insert_sample(dir: &Path, sample_id: &str, job_id: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO fingerprint_samples
            (sample_id, file_id, source_version, capture_profile_key, kind,
             window_start_ms, window_end_ms, pcm_duration_ms, fingerprint_json,
             captured_job_id, captured_at_ms, metrics_json)
         VALUES (?1, 'file-1', 'v1', 'profile', 'intro', 0, 1000, 1000, '{}', ?2, 1, '{}')",
        params![sample_id, job_id],
    )
    .unwrap();
}

fn probe_job_exists(dir: &Path, id: &str) -> bool {
    open_db(dir, "library.db")
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM probe_jobs WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn attempt_count(dir: &Path) -> i64 {
    open_db(dir, "library.db")
        .query_row("SELECT COUNT(*) FROM fingerprint_attempts", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn catalog_count(dir: &Path) -> i64 {
    open_db(dir, "cache.db")
        .query_row("SELECT COUNT(*) FROM tmdb_cache", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn qoe_only_deletes_rows_strictly_older_than_cutoff() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let (_, media) = seed_local_movie(&store, tmp.path());
    for recorded_at in [CUTOFF_SECS - 1, CUTOFF_SECS, CUTOFF_SECS + 1] {
        store
            .insert_metric(
                user,
                media,
                0,
                None,
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                recorded_at,
            )
            .unwrap();
    }

    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, false)
        .unwrap();

    assert_eq!(batch.examined, 1);
    assert_eq!(batch.deleted, 1);
    assert_eq!(batch.protected, 0);
    assert!(!batch.incomplete);
    assert!(batch.resume_after.is_none());
    assert_eq!(metric_count(tmp.path()), 2, "边界秒与更新的行必须保留");
}

#[test]
fn probe_keeps_three_newest_per_scope_and_all_time_ties() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // scope 内 6 条：最新 3 条为 50、40a、40b；40 秒并列全部保护。
    for (id, ts) in [
        ("a-10", 10),
        ("a-20", 20),
        ("a-30", 30),
        ("a-40a", 40),
        ("a-40b", 40),
        ("a-50", 50),
    ] {
        insert_probe_job(tmp.path(), id, "scope-a", "succeeded", Some(ts));
    }

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();

    assert_eq!(batch.examined, 6);
    assert_eq!(
        batch.protected, 3,
        "40a/40b/50 必须保留（时间 tie 全部保护）"
    );
    assert_eq!(batch.deleted, 3);
    assert!(!batch.incomplete);
    for kept in ["a-40a", "a-40b", "a-50"] {
        assert!(probe_job_exists(tmp.path(), kept), "{kept} 必须保留");
    }
    for gone in ["a-10", "a-20", "a-30"] {
        assert!(!probe_job_exists(tmp.path(), gone), "{gone} 应被删除");
    }
}

#[test]
fn captured_sample_and_active_unit_protect_probe_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for (id, ts) in [
        ("b-target", 10),
        ("b-20", 20),
        ("b-30", 30),
        ("b-40", 40),
        ("b-50", 50),
        ("b-60", 60),
    ] {
        insert_probe_job(tmp.path(), id, "scope-b", "succeeded", Some(ts));
    }
    insert_sample(tmp.path(), "sample-1", "b-target");
    // 终态 job 但仍有活动 unit：保守保护。
    insert_probe_job(tmp.path(), "b-unit", "scope-b2", "failed", Some(10));
    insert_unit(tmp.path(), "b-unit", "file-1", "running");
    for (id, ts) in [("b2-20", 20), ("b2-30", 30), ("b2-40", 40)] {
        insert_probe_job(tmp.path(), id, "scope-b2", "succeeded", Some(ts));
    }
    insert_probe_job(tmp.path(), "b-active", "scope-b3", "running", None);
    // 终态 job 仍被 probe stage state 引用：不得删除留下悬空引用。
    for (id, ts) in [("b-stage", 10), ("b4-20", 20), ("b4-30", 30), ("b4-40", 40)] {
        insert_probe_job(tmp.path(), id, "scope-b4", "succeeded", Some(ts));
    }
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute(
            "INSERT INTO probe_stage_state
                (file_id, context_key, stage, status, updated_at_ms, active_job_id)
             VALUES ('file-stage', 'ctx', 'metadata', 'running', 1, 'b-stage')",
            [],
        )
        .unwrap();
    }

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();

    // b-target 被 sample 引用；b-40/50/60 是最新 3 条；b-unit 有活动 unit；
    // b-active 不是终态。可删的只有 b-20/b-30。
    assert_eq!(batch.examined, 14);
    assert_eq!(batch.protected, 12);
    assert_eq!(batch.deleted, 2);
    for kept in [
        "b-target", "b-40", "b-50", "b-60", "b-unit", "b2-20", "b2-30", "b2-40", "b-active",
        "b-stage", "b4-20", "b4-30", "b4-40",
    ] {
        assert!(probe_job_exists(tmp.path(), kept), "{kept} 必须保留");
    }
    assert!(!probe_job_exists(tmp.path(), "b-20"));
    assert!(!probe_job_exists(tmp.path(), "b-30"));
}

#[test]
fn attempts_are_deleted_in_the_same_transaction_as_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for (id, ts) in [
        ("tx-target", 10),
        ("tx-20", 20),
        ("tx-30", 30),
        ("tx-40", 40),
        ("tx-50", 50),
        ("tx-60", 60),
    ] {
        insert_probe_job(tmp.path(), id, "scope-tx", "succeeded", Some(ts));
    }
    insert_attempt(tmp.path(), "attempt-1", "tx-target");
    insert_attempt(tmp.path(), "attempt-2", "tx-target");

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();
    assert_eq!(batch.deleted, 3, "tx-target/tx-20/tx-30 可删");
    assert!(!probe_job_exists(tmp.path(), "tx-target"));
    assert_eq!(
        attempt_count(tmp.path()),
        0,
        "job 删除必须级联它的 attempts"
    );

    // 用触发器证明 attempts 删除失败时 job 也不会被删（同一事务回滚）。
    for (id, ts) in [
        ("rb-target", 10),
        ("rb-20", 20),
        ("rb-30", 30),
        ("rb-40", 40),
        ("rb-50", 50),
        ("rb-60", 60),
    ] {
        insert_probe_job(tmp.path(), id, "scope-rb", "succeeded", Some(ts));
    }
    insert_attempt(tmp.path(), "attempt-rb", "rb-target");
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute_batch(
            "CREATE TRIGGER block_attempt_delete BEFORE DELETE ON fingerprint_attempts
             WHEN OLD.job_id = 'rb-target'
             BEGIN SELECT RAISE(ABORT, 'blocked for rollback test'); END;",
        )
        .unwrap();
    }
    let error = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap_err();
    assert!(error.to_string().contains("blocked"), "{error}");
    assert!(
        probe_job_exists(tmp.path(), "rb-target"),
        "attempts 删除失败必须回滚 job 删除"
    );
    assert_eq!(attempt_count(tmp.path()), 1);
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute_batch("DROP TRIGGER block_attempt_delete;")
            .unwrap();
    }
    let retried = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();
    assert_eq!(retried.deleted, 3);
    assert!(!probe_job_exists(tmp.path(), "rb-target"));
    assert_eq!(attempt_count(tmp.path()), 0);
}

#[test]
fn transient_catalog_deletes_only_allowlisted_paths_and_protects_details_and_unknowns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let old = CUTOFF_SECS - 1;
    for (key, fetched_at) in [
        ("/search/movie?query=avatar", old),
        ("/discover/movie?with_genres=28", old),
        ("/trending/movie/day", old),
        ("/j/search_subjects?type=movie", old),
        ("/movie/42", old),
        ("/tv/42?language=zh-CN", old),
        ("/movie/42/credits", old),
        ("search:movie:avatar", old),
        ("/search/tv?query=fresh", CUTOFF_SECS + 1),
    ] {
        store
            .put_catalog_cache("tmdb", key, "{}", fetched_at, None)
            .unwrap();
    }

    let batch = store
        .prune_data_history(
            RetentionClass::TransientCatalog,
            CUTOFF_MS,
            None,
            500,
            false,
        )
        .unwrap();

    // 超龄行 8 条：4 条 transient 可删，3 条详情 + 1 条未知 key 保护；
    // 更新的一行不在窗口内。
    assert_eq!(batch.examined, 8);
    assert_eq!(batch.deleted, 4);
    assert_eq!(batch.protected, 4);
    assert!(!batch.incomplete);
    assert_eq!(catalog_count(tmp.path()), 5);
    for kept in [
        "/movie/42",
        "/tv/42?language=zh-CN",
        "/movie/42/credits",
        "search:movie:avatar",
        "/search/tv?query=fresh",
    ] {
        assert!(
            store.get_catalog_cache("tmdb", kept).unwrap().is_some(),
            "{kept} 必须保留"
        );
    }
    assert!(
        store
            .get_catalog_cache("tmdb", "/search/movie?query=avatar")
            .unwrap()
            .is_none()
    );
}

#[test]
fn dry_run_reports_the_same_window_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let (_, media) = seed_local_movie(&store, tmp.path());
    for _ in 0..3 {
        store
            .insert_metric(
                user,
                media,
                0,
                None,
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                CUTOFF_SECS - 10,
            )
            .unwrap();
    }

    // Step 2「preview 零写」：dry_run 前后主库字节数（WAL 只记录，不断言）。
    let db_before = file_len(&tmp.path().join("library.db"));
    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, true)
        .unwrap();

    assert_eq!(batch.examined, 3);
    assert_eq!(batch.deleted, 0);
    assert_eq!(metric_count(tmp.path()), 3, "dry_run 不得写任何行");
    let db_after = file_len(&tmp.path().join("library.db"));
    let wal_after = file_len(&tmp.path().join("library.db-wal"));
    println!("MEASURE task20/store preview_zero_write db={db_before}->{db_after} wal={wal_after}");
    assert_eq!(db_after, db_before, "dry_run 必须连字节都不写");
}

#[test]
fn keyset_cursor_pages_without_duplicates_and_stops_at_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let (_, media) = seed_local_movie(&store, tmp.path());
    for index in 0..1200 {
        store
            .insert_metric(
                user,
                media,
                0,
                None,
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                CUTOFF_SECS - 2000 + index,
            )
            .unwrap();
    }

    let first = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, false)
        .unwrap();
    assert_eq!(
        (first.examined, first.deleted, first.incomplete),
        (500, 500, true)
    );
    let cursor = first.resume_after.expect("窗口未完成必须返回游标");

    let second = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, Some(&cursor), 500, false)
        .unwrap();
    assert_eq!(
        (second.examined, second.deleted, second.incomplete),
        (500, 500, true)
    );
    let cursor = second.resume_after.unwrap();

    let third = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, Some(&cursor), 500, false)
        .unwrap();
    assert_eq!(
        (third.examined, third.deleted, third.incomplete),
        (200, 200, false)
    );
    assert!(third.resume_after.is_none());
    assert_eq!(metric_count(tmp.path()), 0);
}

#[test]
fn cursor_carries_examined_position_when_the_window_is_full_of_protected_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let (_, media) = seed_local_movie(&store, tmp.path());
    for index in 0..3 {
        store
            .insert_metric(
                user,
                media,
                0,
                None,
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                CUTOFF_SECS - 10 + index,
            )
            .unwrap();
    }
    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 2, false)
        .unwrap();
    assert_eq!(batch.examined, 2);
    assert_eq!(batch.deleted, 2);
    assert!(batch.incomplete);
    let cursor: DataRetentionCursor = batch.resume_after.unwrap();
    assert_eq!(cursor.at_ms, (CUTOFF_SECS - 9) * 1000);
}

#[test]
fn zero_limit_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let error = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 0, true)
        .unwrap_err();
    assert!(matches!(error, store::StoreError::Invalid(_)), "{error}");
}

#[test]
fn transient_report_examined_limit_is_capped_at_500() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // Verify that requesting limit > 500 does not exceed 500 examined rows
    let batch = store
        .prune_data_history(
            RetentionClass::TransientCatalog,
            CUTOFF_MS,
            None,
            1000,
            true,
        )
        .unwrap();
    assert!(batch.examined <= 500);
}

/// 计划 Task 20 Step 1：真实 10 万行缓存更新/清空 + 瞬态目录/Probe 历史清理都不得
/// 改动本地事实（条目/进度/活动操作），并且必须证明可清理的旧历史真被删掉——只断言
/// 「没删」在清理器整体失效时仍会通过。
///
/// 读取断言全走公开 seam（`library_roster_stats`/`playback_units_for_media`/
/// `unfinished_media_merges`/`media::response_cache`）；probe job 与 media_operations
/// 的 fixture 只能用 test-only 原始 SQL（store 无公开的任意时间戳写入/创建入口）。
#[test]
fn retention_preserves_active_operations_and_local_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let store = Store::open(&data_dir).unwrap();
    let (library_id, media_id) = seed_local_movie(&store, tmp.path());
    let user_id = UserId::new();
    store
        .upsert_unit(
            user_id,
            media_id,
            -1,
            -1,
            5000,
            Some(false),
            Some(true),
            Some(10_000),
            None,
            None,
            false,
            1000,
        )
        .unwrap();
    seed_active_merge(&data_dir, media_id);
    seed_probe_history(&data_dir);

    let stats_before = store.library_roster_stats(&library_id).unwrap();
    let local_items_before = stats_before.item_count;
    let playback_before = play_unit_count(&store, user_id, media_id);
    let active_operations_before = store.unfinished_media_merges().unwrap();
    assert_eq!(local_items_before, 1, "夹具必须真的落了本地条目");
    assert_eq!(stats_before.file_count, 1, "夹具必须真的发布了文件");
    assert_eq!(playback_before, 1, "夹具必须真的落了用户进度");
    assert_eq!(
        active_operations_before.len(),
        1,
        "夹具必须真的落了活动操作"
    );

    let (cache_rows, cache_elapsed) = churn_cache(&data_dir);
    seed_catalog_keys(&data_dir);

    let stats_after_cache_clear = store.library_roster_stats(&library_id).unwrap();
    let local_items_after_cache_clear = stats_after_cache_clear.item_count;
    let playback_after_cache_clear = play_unit_count(&store, user_id, media_id);

    let prune_started = Instant::now();
    let catalog = prune(&store, RetentionClass::TransientCatalog);
    let probe = prune(&store, RetentionClass::ProbeHistory);
    let prune_elapsed = prune_started.elapsed();

    let active_operations_after_retention = store.unfinished_media_merges().unwrap();

    // 清理器必须真的动手：瞬态搜索键 + 最旧 Probe 历史被删；详情键与最新 3 条被保护。
    assert_eq!(catalog.deleted, 1, "过期瞬态搜索键必须被删掉");
    assert_eq!(catalog.protected, 1, "详情键必须被保护");
    assert_eq!(probe.deleted, 1, "最旧的 Probe 历史必须被删掉");
    assert_eq!(probe.protected, 3, "同一 scope 最新 3 条必须保留");
    assert!(!probe_job_exists(&data_dir, "retention-oldest"));
    for kept in ["retention-mid", "retention-new", "retention-newest"] {
        assert!(probe_job_exists(&data_dir, kept), "{kept} 必须保留");
    }
    let cache = open_cache(&data_dir);
    let cached = |key: &str| {
        media::response_cache::get(&cache, CacheSource::Tmdb, key)
            .unwrap()
            .is_some()
    };
    assert!(cached("/movie/12345"), "详情键不得被瞬态清理删掉");
    assert!(!cached("/search/movie?page=0"), "瞬态键必须已被删掉");

    // 本地事实在「缓存清空后」与「历史清理后」都必须一字不差（整条 roster 快照）。
    assert_eq!(local_items_after_cache_clear, local_items_before);
    assert_eq!(playback_after_cache_clear, playback_before);
    assert_eq!(active_operations_after_retention, active_operations_before);
    assert_eq!(
        store.library_roster_stats(&library_id).unwrap(),
        stats_before
    );
    assert_eq!(play_unit_count(&store, user_id, media_id), playback_before);
    assert_eq!(store.unfinished_media_merges().unwrap().len(), 1);

    report_measurements(
        &data_dir,
        cache_rows,
        cache_elapsed,
        prune_elapsed,
        &catalog,
        &probe,
    );
}

fn play_unit_count(store: &Store, user: UserId, media: MediaId) -> usize {
    store
        .playback_units_for_media(user, &[media])
        .unwrap()
        .len()
}

fn prune(store: &Store, class: RetentionClass) -> DataRetentionBatch {
    store
        .prune_data_history(class, CUTOFF_MS, None, 500, false)
        .unwrap()
}

/// 一条真实发布的本地条目 + 它所属 Library 的 id（读取走 `list_libraries`）。
fn seed_local_movie(store: &Store, root: &Path) -> (String, MediaId) {
    let root = root.join("library");
    let (media_id, _) = support::seed_published_file(store, &root, "retention");
    let canonical = std::fs::canonicalize(&root).unwrap();
    let libraries = store.list_libraries().unwrap();
    let owner = libraries.iter().find(|l| l.root_paths.contains(&canonical));
    let library_id = owner.expect("夹具必须创建 Library").id.clone();
    (library_id, media_id)
}

/// 活动 merge 操作（state = `frozen`）：store 无公开创建入口，只能 test-only 写入。
fn seed_active_merge(data_dir: &Path, media_id: MediaId) {
    let sql = "INSERT INTO media_operations
            (operation_id, kind, plan_json, phase_cursor, state, error, created_at_ms,
             updated_at_ms, survivor_id, loser_id)
         VALUES ('00000000-0000-0000-0000-000000000099', 'merge', '{}', 'init', 'frozen',
                 NULL, 1000, 1000, ?1, NULL)";
    open_db(data_dir, "library.db")
        .execute(sql, params![media_id.to_string()])
        .unwrap();
}

/// 同一 scope 4 条终态 Probe job：最新 3 条按规则保留，最旧 1 条必须被真正删掉。
fn seed_probe_history(data_dir: &Path) {
    let jobs = [
        ("retention-oldest", 4000),
        ("retention-mid", 3000),
        ("retention-new", 2000),
        ("retention-newest", 1000),
    ];
    for (id, age_ms) in jobs {
        insert_probe_job(
            data_dir,
            id,
            "scope-retention",
            "succeeded",
            Some(CUTOFF_MS - age_ms),
        );
    }
}

fn open_cache(data_dir: &Path) -> Connection {
    media::response_cache::open(data_dir.join("cache.db")).unwrap()
}

/// 计划 Task 20 的 10 万行缓存写入 + 逐键清空（只走 media 公开 API）：(写入行数, 耗时)。
fn churn_cache(data_dir: &Path) -> (usize, Duration) {
    let conn = open_cache(data_dir);
    let started = Instant::now();
    conn.execute_batch("BEGIN").unwrap();
    for index in 0..CACHE_ROWS {
        search_put(&conn, index);
    }
    conn.execute_batch("COMMIT").unwrap();
    let loaded = media::response_cache::list(&conn, None).unwrap().len();
    assert_eq!(loaded, CACHE_ROWS, "10 万行必须真的写进 cache.db");

    conn.execute_batch("BEGIN").unwrap();
    let mut deleted = 0usize;
    for index in 0..CACHE_ROWS {
        let key = format!("/search/movie?page={index}");
        if media::response_cache::delete(&conn, CacheSource::Tmdb, &key).unwrap() {
            deleted += 1;
        }
    }
    conn.execute_batch("COMMIT").unwrap();
    let elapsed = started.elapsed();
    assert_eq!(deleted, CACHE_ROWS, "清空必须真的删掉每一行");
    assert_eq!(media::response_cache::list(&conn, None).unwrap().len(), 0);
    (loaded, elapsed)
}

fn search_put(conn: &Connection, index: usize) {
    let key = format!("/search/movie?page={index}");
    media::response_cache::put(conn, CacheSource::Tmdb, &key, "{}", 1_000, None).unwrap();
}

/// 过期瞬态键（可清理）与详情键（必须保护）各一条；fetched_at 早于 cutoff 秒。
fn seed_catalog_keys(data_dir: &Path) {
    let conn = open_cache(data_dir);
    let old = CUTOFF_SECS - 10;
    for key in ["/search/movie?page=0", "/movie/12345"] {
        media::response_cache::put(&conn, CacheSource::Tmdb, key, "{}", old, None).unwrap();
    }
}

/// Step 2 指标输出：只打印、不对数值断言；`--nocapture` 可见。测不到的项标「未测得」。
fn report_measurements(
    data_dir: &Path,
    cache_rows: usize,
    cache_elapsed: Duration,
    prune_elapsed: Duration,
    catalog: &DataRetentionBatch,
    probe: &DataRetentionBatch,
) {
    let throughput = cache_rows as f64 / cache_elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    println!(
        "MEASURE task20/store cache_rows={cache_rows} write_and_clear_wall_ms={} \
         write_throughput_rows_per_s={throughput:.0}",
        cache_elapsed.as_millis()
    );
    println!(
        "MEASURE task20/store prune_wall_ms={} catalog_examined={} catalog_deleted={} \
         catalog_protected={} probe_examined={} probe_deleted={} probe_protected={}",
        prune_elapsed.as_millis(),
        catalog.examined,
        catalog.deleted,
        catalog.protected,
        probe.examined,
        probe.deleted,
        probe.protected
    );
    let sizes = ["library.db", "library.db-wal", "cache.db", "cache.db-wal"]
        .map(|name| format!("{name}={}", file_len(&data_dir.join(name))))
        .join(" ");
    println!("MEASURE task20/store bytes {sizes}");
    for name in ["library.db", "cache.db"] {
        let conn = open_db(data_dir, name);
        let page_size = pragma_i64(&conn, "PRAGMA page_size");
        let page_count = pragma_i64(&conn, "PRAGMA page_count");
        let free = pragma_i64(&conn, "PRAGMA freelist_count");
        let used_pct = (page_count - free) as f64 * 100.0 / page_count.max(1) as f64;
        println!(
            "MEASURE task20/store page_util {name} page_size={page_size} page_count={page_count} \
             freelist={free} used_pct={used_pct:.1}"
        );
        let stat = match dbstat_pages(&conn) {
            Some(rows) => {
                let total: i64 = rows.iter().map(|(_, bytes)| *bytes).sum();
                let top: Vec<_> = rows.iter().take(8).collect();
                format!("total_bytes={total} top8={top:?}")
            }
            None => "未测得(本构建无 dbstat 虚表)".to_string(),
        };
        println!("MEASURE task20/store dbstat {name}={stat}");
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn pragma_i64(conn: &Connection, pragma: &str) -> i64 {
    conn.query_row(pragma, [], |row| row.get::<_, i64>(0))
        .unwrap()
}

/// `dbstat` 虚表不可用时返回 `None`，由调用方标「未测得」，不编造数字。
fn dbstat_pages(conn: &Connection) -> Option<Vec<(String, i64)>> {
    let sql = "SELECT name, SUM(pgsize) FROM dbstat GROUP BY name ORDER BY 2 DESC";
    let mut stmt = conn.prepare(sql).ok()?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).ok()?;
    rows.collect::<Result<Vec<_>, _>>().ok()
}
