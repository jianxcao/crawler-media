//! DB-05 控制面：默认关闭/dry_run、禁用批边界、配置 CAS、预览零写入、
//! 200k 历史的有界批报告与游标续跑。

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use api::ApiState;
use api::maintenance::{MaintenancePolicy, run, seed_maintenance_defs};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use jobs::{JobKind, PayloadUpdate};
use rusqlite::params;
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{DAY, temp_state, unix_now};

fn data_dir(state: &ApiState) -> std::path::PathBuf {
    support::data_dir(state)
}

fn retention_def(state: &ApiState) -> jobs::JobDef {
    state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == JobKind::DataRetention)
        .expect("默认注册必须包含 data_retention")
}

fn enable_with_payload(state: &ApiState, def: &jobs::JobDef, payload: &Value) {
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

fn insert_old_metrics(state: &ApiState, count: usize, recorded_at: i64) {
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

fn metric_count(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("library.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM playback_metrics", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn table_count(state: &ApiState, db: &str, table: &str) -> i64 {
    rusqlite::Connection::open(data_dir(state).join(db))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn cursor_settings(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("app.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key LIKE 'maintenance.data_retention.cursor.%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn claim_for(state: &ApiState, def_id: domain::JobDefId, now: i64) -> jobs::Job {
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

fn real_payload() -> Value {
    json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 90,
        "transient_cache_days": 90,
        "probe_days": 90,
    })
}

#[test]
fn new_data_retention_is_disabled_and_dry_run() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);

    assert!(!def.enabled, "新类别默认关闭，生产启用需用户显式确认");
    assert_eq!(def.kind.as_str(), "data_retention");
    assert!(def.kind.is_maintenance());
    assert_eq!(
        def.schedule,
        Some(jobs::Schedule::Interval { secs: 86_400 })
    );

    match MaintenancePolicy::parse(def.kind, &def.payload).unwrap() {
        MaintenancePolicy::DataRetention(policy) => {
            assert_eq!(policy.version, 1);
            assert!(policy.dry_run);
            assert_eq!(policy.qoe_days, 90);
            assert_eq!(policy.transient_cache_days, 90);
            assert_eq!(policy.probe_days, 90);
        }
        other => panic!("data_retention payload 解析成了错误策略: {other:?}"),
    }
}

#[test]
fn invalid_version_or_range_is_rejected() {
    let kind = JobKind::DataRetention;
    let valid = json!({
        "version": 1, "dry_run": true,
        "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90,
    });
    assert!(MaintenancePolicy::parse(kind, &valid.to_string()).is_ok());

    for invalid in [
        json!({"version": 2, "dry_run": true, "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 0, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 36_501, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90, "extra": 1}),
    ] {
        assert!(
            MaintenancePolicy::parse(kind, &invalid.to_string()).is_err(),
            "必须拒绝: {invalid}"
        );
    }
}

#[test]
fn restart_does_not_overwrite_user_config() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);
    let custom = json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 30,
        "transient_cache_days": 7,
        "probe_days": 365,
    });
    enable_with_payload(&state, &def, &custom);

    {
        let queue = state.jobs();
        let guard = queue.lock();
        seed_maintenance_defs(&guard).unwrap();
    }

    let after = state.jobs().lock().def(def.id).unwrap().unwrap();
    assert!(after.enabled, "重启不得覆盖用户启用状态");
    assert_eq!(
        serde_json::from_str::<Value>(&after.payload).unwrap(),
        custom,
        "重启不得覆盖用户策略"
    );
    assert_eq!(
        after.schedule,
        Some(jobs::Schedule::Interval { secs: 86_400 })
    );
}

#[test]
fn transient_report_examined_bounded_by_500() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let job = claim_for(&state, def.id, now);
    run(&state, &job).unwrap();
    let report = support::progress_of(&state, &job);

    for class_report in report["classes"].as_array().unwrap() {
        if class_report["class"] == "transient_catalog" {
            let examined = class_report["examined"].as_u64().unwrap();
            assert!(examined <= 500, "examined must be <= 500 per batch");
        }
    }
}

#[test]
fn disable_after_first_batch_stops_second() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 10_000, now - 100 * DAY);
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let job = claim_for(&state, def.id, now);
    let state_clone = state.clone();
    let monitor = thread::spawn(move || {
        let conn = rusqlite::Connection::open(data_dir(&state_clone).join("library.db")).unwrap();
        for _ in 0..5_000 {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM playback_metrics WHERE id LIKE 'old-%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if count < 10_000 {
                state_clone
                    .jobs()
                    .lock()
                    .set_def_enabled_by_id(def.id, false)
                    .unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("monitor 未观察到第一批删除");
    });

    run(&state, &job).unwrap();
    monitor.join().unwrap();

    let report = support::progress_of(&state, &job);
    let batches = report["batches"].as_u64().unwrap();
    assert!(batches >= 1, "至少完成第一批: {report}");
    assert!(batches < 20, "禁用后必须提前停止: {report}");
    assert_eq!(
        report["deleted"].as_u64().unwrap(),
        batches * 500,
        "已提交批次的删除数量必须一致: {report}"
    );
    assert!(
        report["note"].as_str().unwrap_or("").contains("禁用"),
        "报告需说明因任务定义禁用而停止: {report}"
    );
    assert!(metric_count(&state) > 0, "必须留下未删除的超龄行");
}

#[test]
fn two_hundred_thousand_qoe_history_reports_bounded_batches_and_resumes() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 200_000, now - 100 * DAY);
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let first = claim_for(&state, def.id, now);
    run(&state, &first).unwrap();
    let report = support::progress_of(&state, &first);

    assert_eq!(report["batches"], 20, "单轮批数上限 20: {report}");
    assert_eq!(report["examined"], 10_000, "20 × 500 窗口: {report}");
    assert_eq!(report["deleted"], 10_000, "20 × 500 删除: {report}");
    assert_eq!(
        report["incomplete"], true,
        "200k 历史不可能一轮完成: {report}"
    );
    assert_eq!(
        report["remaining"], true,
        "预算不足必须显式 remaining: {report}"
    );
    assert_eq!(report["budget_exhausted"], true);
    let qoe = &report["classes"][0];
    assert_eq!(qoe["class"], "qoe");
    assert_eq!(qoe["incomplete"], true);
    assert!(qoe["resume_after"].is_object(), "必须返回续跑游标: {qoe}");
    assert_eq!(metric_count(&state), 190_000);

    state.jobs().lock().succeed_claimed(&first, now).unwrap();
    let second = claim_for(&state, def.id, now);
    run(&state, &second).unwrap();
    let report = support::progress_of(&state, &second);
    assert_eq!(
        report["deleted"], 10_000,
        "持久游标必须继续下一窗口: {report}"
    );
    assert_eq!(metric_count(&state), 180_000);
}

#[tokio::test]
async fn preview_is_read_only_and_reports_classes() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 3, now - 100 * DAY);
    {
        let store = state.store();
        let store = store.lock();
        store
            .put_catalog_cache(
                "tmdb",
                "/search/movie?query=avatar",
                "{}",
                now - 100 * DAY,
                None,
            )
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(data_dir(&state).join("library.db")).unwrap();
        for (id, ts) in [("p-10", 10), ("p-20", 20), ("p-30", 30), ("p-40", 40)] {
            conn.execute(
                "INSERT INTO probe_jobs
                    (id, kind, media_id, season, scope_key, status, total, completed,
                     succeeded, failed, partial, error, created_at_ms, started_at_ms, finished_at_ms)
                 VALUES (?1, 'metadata', 'm', NULL, 'scope-p', 'succeeded', 1, 1, 1, 0, 0, NULL, ?2, ?2, ?2)",
                params![id, ts],
            )
            .unwrap();
        }
    }
    let def = retention_def(&state);
    let app = api::router(state.clone());
    let token = support::TOKEN;

    // 预览请求故意带 dry_run=false：预览必须自己强制只读，不能信客户端开关。
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/jobs/{}/maintenance-preview", def.id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(
                    json!({
                        "payload": {
                            "version": 1, "dry_run": false,
                            "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90,
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let data = &envelope["data"];
    assert_eq!(data["examined"], 8, "3 QoE + 1 catalog + 4 probe: {data}");
    assert_eq!(data["truncated"], false);
    assert_eq!(data["classes"].as_array().unwrap().len(), 3);

    assert_eq!(metric_count(&state), 3, "预览不得删除 QoE 行");
    assert_eq!(table_count(&state, "cache.db", "tmdb_cache"), 1);
    assert_eq!(table_count(&state, "library.db", "probe_jobs"), 4);
    assert_eq!(cursor_settings(&state), 0, "预览不得写游标");
}

#[tokio::test]
async fn maintenance_config_accepts_data_retention_and_enforces_cas() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);
    let expected: Value = serde_json::from_str(&def.payload).unwrap();
    let app = api::router(state.clone());
    let token = support::TOKEN;
    let valid = json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 30,
        "transient_cache_days": 14,
        "probe_days": 120,
    });

    let put = |body: Value| {
        let app = app.clone();
        let uri = format!("/api/v1/jobs/{}/maintenance-config", def.id);
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };

    let (status, response) = put(json!({
        "payload": valid.clone(),
        "expected_payload": expected.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["data"]["payload"]["qoe_days"], 30);

    // 旧令牌再次提交 → 409，不允许覆盖。
    let (status, _) = put(json!({
        "payload": valid.clone(),
        "expected_payload": expected.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 越界窗口 → 400，且不落库。
    let (status, _) = put(json!({
        "payload": {
            "version": 1, "dry_run": false,
            "qoe_days": 0, "transient_cache_days": 14, "probe_days": 120,
        },
        "expected_payload": valid.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

const CACHE_ROWS: usize = 100_000;
/// 与 `real_payload()` 的 `probe_days` 对齐，种子必须落在保留窗口之外。
const RETENTION_DAYS: i64 = 90;

/// 夹具：真实登记 Media、经公开 `publish_files` 落一个已发布文件、写一条播放进度。
/// 返回的 (user, media) 之后一律用公开 seam 复核，不读私有 SQL。
fn seed_local_facts(state: &ApiState, root: &std::path::Path) -> (domain::UserId, domain::MediaId) {
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

fn fixture_library_id(state: &ApiState) -> String {
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

fn library_stats(state: &ApiState) -> store::LibraryRosterStats {
    let library_id = fixture_library_id(state);
    state
        .store()
        .lock()
        .library_roster_stats(&library_id)
        .unwrap()
}

fn playback_count(state: &ApiState, user: domain::UserId, media: domain::MediaId) -> usize {
    state
        .store()
        .lock()
        .playback_units_for_media(user, &[media])
        .unwrap()
        .len()
}

fn active_operations(state: &ApiState) -> usize {
    state
        .store()
        .lock()
        .unfinished_media_merges()
        .unwrap()
        .len()
}

/// 公开 API 没有创建 `media_operations` 行的入口（`begin_merge` 是 crate 私有），
/// 活动操作只能用种子 SQL 造出来；读取一律走 `unfinished_media_merges()`。
fn seed_active_merge(state: &ApiState, media: domain::MediaId) {
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
fn seed_probe_history(state: &ApiState, now: i64) {
    let conn = rusqlite::Connection::open(data_dir(state).join("library.db")).unwrap();
    let cutoff_ms = (now - RETENTION_DAYS * DAY) * 1000;
    for (id, age_ms) in [
        ("retention-oldest", 4 * DAY * 1000),
        ("retention-mid", 3 * DAY * 1000),
        ("retention-new", 2 * DAY * 1000),
        ("retention-newest", 1 * DAY * 1000),
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

fn probe_job_exists(state: &ApiState, id: &str) -> bool {
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

fn open_cache(state: &ApiState) -> rusqlite::Connection {
    media::response_cache::open(data_dir(state).join("cache.db")).unwrap()
}

/// 计划 Task 20 的 10 万行缓存写入 + 逐键清空，全部走 media 公开 API 写真实 `cache.db`。
/// 返回 (写入成功的行数, 墙钟耗时)；规模由调用点断言，避免「缩到 10 行冒充」。
fn churn_cache(state: &ApiState) -> (usize, Duration) {
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

fn response_cache_rows(state: &ApiState) -> usize {
    media::response_cache::list(&open_cache(state), None)
        .unwrap()
        .len()
}

/// 超龄瞬态搜索键（可清理）+ 详情键（必须保护），写入真实 `cache.db`。
fn seed_catalog_keys(state: &ApiState, now: i64) {
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

fn cached(state: &ApiState, key: &str) -> bool {
    media::response_cache::get(
        &open_cache(state),
        media::response_cache::CacheSource::Tmdb,
        key,
    )
    .unwrap()
    .is_some()
}

fn class_report<'a>(report: &'a Value, class: &str) -> &'a Value {
    report["classes"]
        .as_array()
        .unwrap_or_else(|| panic!("报告缺少 classes 数组: {report}"))
        .iter()
        .find(|entry| entry["class"] == class)
        .unwrap_or_else(|| panic!("报告缺少 {class} 类别: {report}"))
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn pragma_i64(state: &ApiState, db: &str, pragma: &str) -> i64 {
    rusqlite::Connection::open(data_dir(state).join(db))
        .unwrap()
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
        .unwrap()
}

/// 计划 Task 20 Step 2 的原始数字：缓存写吞吐、清理耗时/计数、DB/WAL 字节、页利用率。
fn report_measurements(
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

#[test]
fn retention_preserves_active_operations_and_local_facts() {
    let (tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    let (user_id, media_id) = seed_local_facts(&state, tmp.path());
    seed_active_merge(&state, media_id);
    seed_probe_history(&state, now);

    // 本地事实（条目/进度/活动操作）先经公开 seam 快照，之后逐条对比。
    let stats_before = library_stats(&state);
    let playback_before = playback_count(&state, user_id, media_id);
    let operations_before = active_operations(&state);
    assert_eq!(stats_before.item_count, 1, "夹具必须真的落了本地条目");
    assert_eq!(stats_before.file_count, 1, "夹具必须真的落了已发布文件");
    assert_eq!(playback_before, 1, "夹具必须真的留下播放进度");
    assert_eq!(operations_before, 1, "夹具必须真的留下一个活动操作");

    // 10 万行缓存更新 + 逐键清空：真库、真规模，只走 media 公开 API。
    let churn = churn_cache(&state);
    assert_eq!(churn.0, CACHE_ROWS, "缓存必须真的写入 10 万行");
    assert_eq!(response_cache_rows(&state), 0, "清空必须真的删掉每一行");
    assert_eq!(
        library_stats(&state),
        stats_before,
        "缓存 10 万次更新/清空不得改动本地条目"
    );
    assert_eq!(
        playback_count(&state, user_id, media_id),
        playback_before,
        "缓存变更不得改动播放进度"
    );

    // 清理目标：超龄瞬态搜索键（必须删）+ 详情键（必须保护）。
    seed_catalog_keys(&state, now);
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let started = Instant::now();
    let job = claim_for(&state, def.id, now);
    run(&state, &job).unwrap();
    let elapsed = started.elapsed();
    let report = support::progress_of(&state, &job);

    let catalog = class_report(&report, "transient_catalog");
    let probe = class_report(&report, "probe_history");
    assert_eq!(catalog["deleted"], 1, "超龄瞬态搜索键必须被真删: {report}");
    assert_eq!(catalog["protected"], 1, "详情键必须受保护: {report}");
    assert_eq!(probe["deleted"], 1, "最旧的 Probe 历史必须被真删: {report}");
    assert_eq!(
        probe["protected"], 3,
        "最新 3 条 Probe 历史必须留下: {report}"
    );
    assert!(
        !probe_job_exists(&state, "retention-oldest"),
        "超龄 Probe 历史必须真的删掉: {report}"
    );
    for kept in ["retention-mid", "retention-new", "retention-newest"] {
        assert!(probe_job_exists(&state, kept), "{kept} 必须留下: {report}");
    }
    assert!(
        !cached(&state, "/search/movie?page=0"),
        "超龄瞬态键不得残留"
    );
    assert!(cached(&state, "/movie/12345"), "详情键不得被清理掉");

    // 清理后本地事实必须逐条不变。
    assert_eq!(library_stats(&state), stats_before, "清理不得改动本地条目");
    assert_eq!(
        playback_count(&state, user_id, media_id),
        playback_before,
        "清理不得改动播放进度"
    );
    assert_eq!(
        active_operations(&state),
        operations_before,
        "活动操作不得被清理带走"
    );

    report_measurements(&state, churn, elapsed, &report);
}
