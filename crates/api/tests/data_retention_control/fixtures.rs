//! `data_retention_control` 的夹具与度量辅助：种子 SQL、行数/游标读取、缓存写入与
//! 报告测量。测试文件只保留用例本身，这里的东西都属于「怎么造事实、怎么读事实」。

use api::ApiState;
use jobs::{JobKind, PayloadUpdate};
use rusqlite::params;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::support;
pub(crate) use crate::support::DAY;
use crate::support::unix_now;

pub(crate) fn data_dir(state: &ApiState) -> std::path::PathBuf {
    support::data_dir(state)
}

pub(crate) fn retention_def(state: &ApiState) -> jobs::JobDef {
    state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == JobKind::DataRetention)
        .expect("默认注册必须包含 data_retention")
}

pub(crate) fn enable_with_payload(state: &ApiState, def: &jobs::JobDef, payload: &Value) {
    let queue = state.jobs();
    let guard = queue.lock();
    assert_eq!(
        guard
            .set_def_payload_cas(def.id, &def.payload, &payload.to_string())
            .unwrap(),
        PayloadUpdate::Updated
    );
    guard.set_def_enabled_by_id(def.id, true).unwrap();
}

pub(crate) fn insert_old_metrics(state: &ApiState, count: usize, recorded_at: i64) {
    let conn = rusqlite::Connection::open(data_dir(state).join("library.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO playback_metrics
                    (id, user_id, media_id, tier, engine, ttff_ms, rebuffer_ms,
                     rebuffer_count, seek_count, dropped_frames, total_frames,
                     watched_ms, recorded_at)
                 VALUES (?1, 'user-1', 'media-1', 0, NULL, NULL, 0, 0, 0, NULL, NULL, 0, ?2)",
            )
            .unwrap();
        for index in 0..count {
            stmt.execute(params![format!("old-{index:06}"), recorded_at])
                .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

pub(crate) fn metric_count(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("library.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM playback_metrics", [], |row| {
            row.get(0)
        })
        .unwrap()
}

pub(crate) fn table_count(state: &ApiState, db: &str, table: &str) -> i64 {
    rusqlite::Connection::open(data_dir(state).join(db))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

pub(crate) fn cursor_settings(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("app.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key LIKE 'maintenance.data_retention.cursor.%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

pub(crate) fn claim_for(state: &ApiState, def_id: domain::JobDefId, now: i64) -> jobs::Job {
    let queue = state.jobs();
    let guard = queue.lock();
    // 首次调用会新建 child；后续调用会复用上一次成功时生成的 follow-up（run_after
    // 被 ensure_scheduled_for(0) 提前到 0）。两条路径都通过 claim 取 running job。
    let created = guard.ensure_scheduled_for(0, def_id).unwrap();
    let claimed = guard.claim(now, 1).unwrap();
    assert_eq!(claimed.len(), 1, "必须正好领取一个维护任务");
    assert_eq!(claimed[0].kind, JobKind::DataRetention);
    if let Some(expected) = created.first() {
        assert_eq!(claimed[0].id, expected.id);
    }
    claimed.into_iter().next().unwrap()
}

pub(crate) fn real_payload() -> Value {
    json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 90,
        "transient_cache_days": 90,
        "probe_days": 90,
    })
}

/// 与 `real_payload()` 的 `probe_days` 对齐，种子必须落在保留窗口之外。
pub(crate) const CACHE_ROWS: usize = 100_000;
pub(crate) const RETENTION_DAYS: i64 = 90;

/// 夹具：真实登记 Media、经公开 `publish_files` 落一个已发布文件、写一条播放进度。
/// 返回的 (user, media) 之后一律用公开 seam 复核，不读私有 SQL。
pub(crate) fn seed_local_facts(
    state: &ApiState,
    root: &std::path::Path,
) -> (domain::UserId, domain::MediaId) {
    let user = domain::UserId::new();
    let media = domain::MediaId::new();
    let store = state.store();
    let guard = store.lock();
    guard
        .insert_media(&domain::Media {
            id: media,
            kind: domain::MediaKind::Movie,
            title: "Retention Movie".into(),
            original_title: None,
            aliases: Vec::new(),
            year: Some(2024),
            tmdb_id: Some("12345".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    support::seed_published_file(&guard, root, media);
    guard
        .upsert_unit(
            user,
            media,
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
    (user, media)
}

pub(crate) fn fixture_library_id(state: &ApiState) -> String {
    state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.name == "Fixture")
        .expect("夹具必须落了 Fixture 库")
        .id
}

pub(crate) fn library_stats(state: &ApiState) -> store::LibraryRosterStats {
    let library_id = fixture_library_id(state);
    state
        .store()
        .lock()
        .library_roster_stats(&library_id)
        .unwrap()
}

pub(crate) fn playback_count(
    state: &ApiState,
    user: domain::UserId,
    media: domain::MediaId,
) -> usize {
    state
        .store()
        .lock()
        .playback_units_for_media(user, &[media])
        .unwrap()
        .len()
}

pub(crate) fn active_operations(state: &ApiState) -> usize {
    state
        .store()
        .lock()
        .unfinished_media_merges()
        .unwrap()
        .len()
}

/// 公开 API 没有创建 `media_operations` 行的入口（`begin_merge` 是 crate 私有），
/// 活动操作只能用种子 SQL 造出来；读取一律走 `unfinished_media_merges()`。
pub(crate) fn seed_active_merge(state: &ApiState, media: domain::MediaId) {
    let conn = rusqlite::Connection::open(data_dir(state).join("library.db")).unwrap();
    conn.execute(
        "INSERT INTO media_operations
            (operation_id, kind, plan_json, phase_cursor, state, error, created_at_ms, updated_at_ms, survivor_id, loser_id)
         VALUES ('00000000-0000-0000-0000-000000000099', 'merge', '{}', 'init', 'frozen', NULL, 1000, 1000, ?1, NULL)",
        params![media.to_string()],
    )
    .unwrap();
}

/// Probe 历史同样没有公开写入口，只能种子。四条都在 `real_payload()` 的 90 天保留窗口
/// 之外（都是候选），其中最新 3 条落在同 scope 的「最新 3 条终态」保护窗口内。
pub(crate) fn seed_probe_history(state: &ApiState, now: i64) {
    let conn = rusqlite::Connection::open(data_dir(state).join("library.db")).unwrap();
    let cutoff_ms = (now - RETENTION_DAYS * DAY) * 1000;
    for (id, age_ms) in [
        ("retention-oldest", 4 * DAY * 1000),
        ("retention-mid", 3 * DAY * 1000),
        ("retention-new", 2 * DAY * 1000),
        ("retention-newest", DAY * 1000),
    ] {
        let finished_at_ms = cutoff_ms - age_ms;
        conn.execute(
            "INSERT INTO probe_jobs
                (id, kind, media_id, season, scope_key, status, total, completed,
                 succeeded, failed, partial, error, created_at_ms, started_at_ms, finished_at_ms)
             VALUES (?1, 'metadata', 'm', NULL, 'scope-retention', 'succeeded', 1, 1, 1, 0, 0, NULL, ?2, ?2, ?2)",
            params![id, finished_at_ms],
        )
        .unwrap();
    }
}

pub(crate) fn probe_job_exists(state: &ApiState, id: &str) -> bool {
    let conn = rusqlite::Connection::open(data_dir(state).join("library.db")).unwrap();
    let rows = conn
        .query_row(
            "SELECT COUNT(*) FROM probe_jobs WHERE id = ?1",
            params![id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    rows > 0
}

pub(crate) fn open_cache(state: &ApiState) -> rusqlite::Connection {
    media::response_cache::open(data_dir(state).join("cache.db")).unwrap()
}

/// 计划 Task 20 的 10 万行缓存写入 + 逐键清空，全部走 media 公开 API 写真实 `cache.db`。
/// 返回 (写入成功的行数, 墙钟耗时)；规模由调用点断言，避免「缩到 10 行冒充」。
pub(crate) fn churn_cache(state: &ApiState) -> (usize, Duration) {
    let conn = open_cache(state);
    let started = Instant::now();
    conn.execute_batch("BEGIN").unwrap();
    for index in 0..CACHE_ROWS {
        media::response_cache::put(
            &conn,
            media::response_cache::CacheSource::Tmdb,
            &format!("/search/movie?page={index}"),
            "{}",
            unix_now() - 200 * DAY,
            None,
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    let loaded = response_cache_rows(state);
    conn.execute_batch("BEGIN").unwrap();
    for index in 0..CACHE_ROWS {
        media::response_cache::delete(
            &conn,
            media::response_cache::CacheSource::Tmdb,
            &format!("/search/movie?page={index}"),
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    (loaded, started.elapsed())
}

pub(crate) fn response_cache_rows(state: &ApiState) -> usize {
    media::response_cache::list(&open_cache(state), None)
        .unwrap()
        .len()
}

/// 超龄瞬态搜索键（可清理）+ 详情键（必须保护），写入真实 `cache.db`。
pub(crate) fn seed_catalog_keys(state: &ApiState, now: i64) {
    let conn = open_cache(state);
    for key in ["/search/movie?page=0", "/movie/12345"] {
        media::response_cache::put(
            &conn,
            media::response_cache::CacheSource::Tmdb,
            key,
            "{}",
            now - 200 * DAY,
            None,
        )
        .unwrap();
    }
}

pub(crate) fn cached(state: &ApiState, key: &str) -> bool {
    media::response_cache::get(
        &open_cache(state),
        media::response_cache::CacheSource::Tmdb,
        key,
    )
    .unwrap()
    .is_some()
}

pub(crate) fn class_report<'a>(report: &'a Value, class: &str) -> &'a Value {
    report["classes"]
        .as_array()
        .unwrap_or_else(|| panic!("报告缺少 classes 数组: {report}"))
        .iter()
        .find(|entry| entry["class"] == class)
        .unwrap_or_else(|| panic!("报告缺少 {class} 类别: {report}"))
}

pub(crate) fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

pub(crate) fn pragma_i64(state: &ApiState, db: &str, pragma: &str) -> i64 {
    rusqlite::Connection::open(data_dir(state).join(db))
        .unwrap()
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
        .unwrap()
}

/// 计划 Task 20 Step 2 的原始数字：缓存写吞吐、清理耗时/计数、DB/WAL 字节、页利用率。
pub(crate) fn report_measurements(
    state: &ApiState,
    churn: (usize, Duration),
    elapsed: Duration,
    report: &Value,
) {
    let dir = data_dir(state);
    println!(
        "MEASURE task20 cache_churn rows={} write_and_clear_wall_ms={} write_throughput_rows_per_s={:.0}",
        churn.0,
        churn.1.as_millis(),
        churn.0 as f64 / churn.1.as_secs_f64()
    );
    println!(
        "MEASURE task20 retention_job wall_ms={} batches={} examined={} deleted={} protected={}",
        elapsed.as_millis(),
        report["batches"],
        report["examined"],
        report["deleted"],
        report["protected"]
    );
    println!(
        "MEASURE task20 bytes library.db={} library.db-wal={} cache.db={} cache.db-wal={}",
        file_len(&dir.join("library.db")),
        file_len(&dir.join("library.db-wal")),
        file_len(&dir.join("cache.db")),
        file_len(&dir.join("cache.db-wal"))
    );
    for db in ["library.db", "cache.db"] {
        let page_size = pragma_i64(state, db, "page_size");
        let page_count = pragma_i64(state, db, "page_count");
        let freelist = pragma_i64(state, db, "freelist_count");
        let used = if page_count > 0 {
            (page_count - freelist) as f64 * 100.0 / page_count as f64
        } else {
            0.0
        };
        println!(
            "MEASURE task20 page_util {db} page_size={page_size} page_count={page_count} freelist={freelist} used_no_freelist_pct={used:.1}"
        );
    }
}
