//! 心跳写作用域与扫描争用的**实测**测试（计划 Task 19 Step 1/2，api 侧）。
//!
//! # 度量口径
//!
//! * `write_scope`：**实测**，且是**行级**语义（task-98 起）。对 `data/library.db` 的每张表，在
//!   操作窗口前后各把全表内容（**含行身份列**）快照进 TEMP 表，再按行身份认行：
//!   `INSERT` = 身份只在窗口后，`DELETE` = 身份只在快照里，`UPDATE` = 身份两边都在但行内容变了。
//!   行身份是 `rowid`；layout v3 的 `WITHOUT ROWID` 表没有 `rowid`，用主键元组
//!   （`storage_contention/harness.rs::DbWatch::identity`），两种形状都满足「同一行前后相同、
//!   不同行不同」。
//!   一行原地 UPDATE 记 **1**（旧的双向 `EXCEPT` 镜像计数会把它记成 2，与「两次 INSERT」不可区分）。
//!   断言同时比 `changed_tables`（按表拆开的 `TableDelta`）和 `write_scope`（不同行总数），
//!   并用正控制 `write_scope_instrument_detects_row_changes` 逐项确认 INSERT / UPDATE / DELETE
//!   三种形状各自被数成多少。
//! * `read_scope`：**未测得**，见 [`READ_SCOPE_UNAVAILABLE`]；下面那条断言只钉住「两侧都是 `None`
//!   （未采集）**未跑**」这个事实，**没有**测到任何读放大，也不构成假绿——将来谁把该字段填上，
//!   那条断言就会失败，必须补成真断言。
//! * 锁等待：**未测得**（没有公开计数器，也不许为此在生产路径加常驻日志）。下面用「扫描批次
//!   持锁时长」和「心跳端到端延迟 p50/p95/max」作为上界；它们是「事务时长 + 排队」，不是纯锁等待。
//!
//! 本文件不改生产路径：无常驻每心跳 `info!`，无第二条写队列，不数 mock 调用次数，也不看 SQL 文本。
//! 度量仪器与夹具按责任拆在 `storage_contention/{harness,fixtures}.rs`，本文件只放测试与断言。

#[path = "management/common.rs"]
mod common;

#[path = "storage_contention/harness.rs"]
mod harness;

#[path = "storage_contention/fixtures.rs"]
mod fixtures;

use std::collections::BTreeMap;
use std::path::PathBuf;

use api::Store;
use domain::{LibraryFile, LibraryId, MediaKind, UnitKey, UserId};
use store::ScanOperationStatus;

use fixtures::*;
use harness::*;

/// 读作用域在这个测试缝上取不到：记录缺的是什么，不用代理量冒充。
const READ_SCOPE_UNAVAILABLE: &str = concat!(
    "read_scope 未测得：集成测试看不到心跳读了多少行。原因是 `StatementStatus` 需要 store 内部持有",
    "的 prepared statement；`rusqlite` 的 `hooks` feature 未开启；`Store::library` 是 pub(crate)；",
    "`#[cfg(test)]` 生产钩子对集成测试不可见。唯一公开的读数（root_ledger_scans / media_ledger_reads /",
    "role_lookups）不覆盖 write_library_summary 里那条按集数增长的 SELECT。要真测到它，需要 store 侧",
    "新增一个 pub 的每-store 心跳读计数器（形如 root_ledger_scans）；该改动落在 crates/store/**，",
    "不在本任务写作用域内，未做。",
);

// A multi-thread runtime is the measurement condition, not a detail: on the
// single-thread runtime a blocking store call parks the only worker, so the
// heartbeat latency below would include the harness's own blocking time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_user_growth_does_not_expand_heartbeat_write_scope() {
    let fixture = fixture("Contention Series");
    let db = fixture.dir.path().join("data/library.db");
    let device = "dev-growth";
    let session = open_play_session(&fixture.app, fixture.series.id, device).await;

    // 预热：第一次心跳会建会话、插摘要行；先做一次，让基线与增长后都是「更新型」提交。
    // 首个事件必须是 `start`（生产客户端顺序），`progress` 是后续心跳的形状。
    beat_start(&fixture.app, fixture.series.id, 1_000, device, &session).await;
    println!("[diag:beat] #1 预热(1000ms) start HTTP 200");

    let watch = DbWatch::open(&db);
    let counters_pre_baseline_beat = read_counters(&fixture.state);
    beat(&fixture.app, fixture.series.id, 30_000, device, &session).await;
    println!("[diag:beat] #2 基线(30000ms) HTTP 200");
    let baseline = watch.finish();
    // 正控制：仪器必须看得见一次真实心跳的写入，否则后面的「相等」毫无意义。
    assert!(
        baseline.write_scope > 0,
        "正控制失败：一次真实心跳没有改动 library.db ⇒ 写作用域仪器失效: {baseline:?}"
    );
    assert!(
        baseline.changed_tables.contains_key("playback_units"),
        "心跳必须落 playback_units: {baseline:?}"
    );

    let unowned_library_revision_before = revision(&fixture.state, &fixture.unowned_library);
    let baseline_beat_delta =
        counter_delta(read_counters(&fixture.state), counters_pre_baseline_beat);
    println!("[diag:counters] baseline_beat_delta={baseline_beat_delta:?}");

    watch.snapshot().unwrap();
    let grown = grow_unrelated_users(&db, fixture.series.id, &fixture.series_library, 100_000);
    assert_eq!(
        grown,
        (100_000, 100_000),
        "10 万行必须真的落库（不是被吞掉的 INSERT 错误）"
    );
    assert_eq!(
        watch.library_changed_rows(&fixture.unowned_library),
        BTreeMap::new(),
        "10 万无关用户增长不得碰这个 Library 的任何一行"
    );

    watch.snapshot().unwrap();
    let counters_pre_growth_beat = read_counters(&fixture.state);
    beat(&fixture.app, fixture.series.id, 60_000, device, &session).await;
    println!("[diag:beat] #3 增长后(60000ms) HTTP 200");
    let after_growth = watch.finish();
    let unowned_library_revision_after = revision(&fixture.state, &fixture.unowned_library);
    let growth_beat_delta = counter_delta(read_counters(&fixture.state), counters_pre_growth_beat);
    println!("[diag:counters] growth_beat_delta={growth_beat_delta:?}");

    // 下面这行**不测任何读放大**：`read_scope` / 锁等待时长在这个缝上**未跑**（原因见
    // READ_SCOPE_UNAVAILABLE，逐字记在证据里）。它只钉住「未采集」这个状态——谁将来把该字段填上，
    // 这行就会失败，必须改成真断言，免得 `None == None` 被当成通过。
    // **这不是通过，是未采集。** plan:425 的 `assert_eq!(after_growth.read_scope, baseline.read_scope)`
    // 在本仪器下不可测 ⇒ 终局记「未通过（不可测）」，不得记为通过。
    // task-98 诊断：写作用域断言会先 panic，看不到逐表行级事实，所以先 dump 再断言。
    println!("[diag:growth] baseline={baseline:?}");
    println!("[diag:growth] after_growth={after_growth:?}");
    let _diag_growth = watch.print_changed_tables("growth");
    watch.print_diff_rows("user_library_items");
    assert_eq!(
        (baseline.read_scope, after_growth.read_scope),
        (None, None),
        "read_scope 未采集（未跑）：本仪器只测写作用域；填上后必须补真断言"
    );
    assert_eq!(after_growth.write_scope, baseline.write_scope);
    assert_eq!(
        unowned_library_revision_after,
        unowned_library_revision_before
    );

    assert_eq!(
        after_growth.changed_tables, baseline.changed_tables,
        "写集合必须逐表相同，不只是总数相同"
    );
    assert_eq!(
        watch.library_changed_rows(&fixture.unowned_library),
        BTreeMap::new(),
        "增长后的心跳也不得改动这个 Library"
    );
    assert_eq!(
        growth_beat_delta, baseline_beat_delta,
        "一次心跳的公开读计数器增量不得随无关用户增长（粗粒度指标，不是 read_scope）"
    );
    println!("READ_SCOPE_UNAVAILABLE: {READ_SCOPE_UNAVAILABLE}");
    println!(
        "baseline={baseline:?}\nafter_growth={after_growth:?}\nbaseline_beat_delta={baseline_beat_delta:?} growth_beat_delta={growth_beat_delta:?}"
    );
}

// Same reason as above: the scan holds the store lock on another thread, and only a
// second worker can keep the heartbeat endpoint answering while it does.
// This performance gate measures release execution; debug instrumentation cost
// cannot be compared with its production latency limits.
#[cfg_attr(
    debug_assertions,
    ignore = "release performance gate; run with --release"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_scan_of_ten_thousand_episodes_keeps_heartbeat_write_scope_bounded() {
    // 规模可调：默认 10k（计划 Step 1 点名的规模）；缩小只为定位写放大点，断言语义不变。
    let episodes: usize = std::env::var("CRAWLER_MEDIA_CONTENTION_EPISODES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);
    const PER_BATCH: usize = 100;

    let fixture = fixture("Scan Series");
    let db = fixture.dir.path().join("data/library.db");
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let device = "dev-scan";
    let session = open_play_session(&fixture.app, fixture.series.id, device).await;

    beat(&fixture.app, fixture.series.id, 1_000, device, &session).await;
    let watch = DbWatch::open(&db);
    beat(&fixture.app, fixture.series.id, 30_000, device, &session).await;
    let baseline = watch.finish();
    assert!(
        baseline.write_scope > 0,
        "正控制：基线心跳必须真的写库: {baseline:?}"
    );

    let wal_before = file_len(&wal);
    let rss_before = rss_kb();
    let plan = ScanPlan {
        library_id: fixture.series_library,
        template: file_template(
            fixture.series_library,
            &fixture.series_root_id,
            fixture.series.id,
            MediaKind::Tv,
        ),
        root: fixture.series_root.clone(),
        first_episode: 2,
        batches: episodes / PER_BATCH,
        per_batch: PER_BATCH,
    };
    let scanner = std::thread::spawn({
        let store = fixture.state.store();
        move || run_scan(store, plan)
    });
    let (latencies, statuses) =
        hammer_while_scanning(&fixture.app, fixture.series.id, device, &session, &scanner).await;
    let report = scanner.join().unwrap();
    let (wal_after, rss_after) = (file_len(&wal), rss_kb());

    assert!(
        report.errors.is_empty(),
        "扫描批次不得报错: {:?}",
        report.errors
    );
    assert_eq!(report.inserted, episodes, "全部集必须整批写入: {report:?}");
    let ok = statuses.get(&200).copied().unwrap_or(0);
    assert_eq!(
        ok,
        latencies.len(),
        "并发扫描期间每个心跳都必须 2xx: {statuses:?}"
    );

    watch.snapshot().unwrap();
    beat(&fixture.app, fixture.series.id, 40_000, device, &session).await;
    let after_scan = watch.finish();
    assert_eq!(
        after_scan.write_scope, baseline.write_scope,
        "同一 Media 长到 10k 集后心跳写作用域不得增长\nbaseline={baseline:?}\nafter_scan={after_scan:?}"
    );
    assert_eq!(
        after_scan.changed_tables, baseline.changed_tables,
        "逐表写集合也必须相同"
    );
    assert_eq!(
        watch.library_changed_rows(&fixture.unowned_library),
        BTreeMap::new(),
        "不拥有该单元的 Library 未被写"
    );

    print_step_two(
        &report,
        &latencies,
        &statuses,
        (wal_before, wal_after),
        (rss_before, rss_after),
        PER_BATCH,
    );
}

#[test]
fn rollback_and_cancelled_scan_keep_facts_and_summary_consistent_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let db = data.join("library.db");
    let store = Store::open(&data).unwrap();

    let (library, template, root_path) = seed_rollback_series(&store, temp.path());
    let library_id: LibraryId = library.id.parse().unwrap();

    // 一条被取消的扫描 + 一条 running 的扫描：只有后者允许在重启后被重新排队。
    let cancelled = store
        .enqueue_scan(&library.id, "sig-a", &[], 1_000)
        .unwrap();
    assert!(
        store.cancel_scan(&cancelled.id, 1_100).unwrap(),
        "取消必须命中活动扫描"
    );
    assert_eq!(scan_status(&db, &cancelled.id), "cancelled");
    let interrupted = store
        .enqueue_scan(&library.id, "sig-b", &[], 1_200)
        .unwrap();
    assert_ne!(interrupted.id, cancelled.id, "取消过的扫描不该被复用");
    assert!(
        store
            .set_scan_operation_status(&interrupted.id, ScanOperationStatus::Running)
            .unwrap()
    );
    assert_eq!(scan_status(&db, &interrupted.id), "running");

    let media_id = template.media_id.to_string();
    let summary_before = summary_tuple(&db, &library.id, &media_id);
    assert!(summary_before.is_some(), "setup 必须已经生成摘要行");
    let watch = DbWatch::open(&db);

    // 真实的事务内失败：同批第 1 个文件合法、第 2 个文件越界 ⇒ 整批回滚。
    let good = episode_file(&template, &root_path, 1, 2);
    let bad = LibraryFile {
        path: PathBuf::from("/outside/root/not-owned.mkv"),
        ..episode_file(&template, &root_path, 1, 3)
    };
    let failed = publish(
        &store,
        &library_id,
        &[good.clone(), bad],
        &[UnitKey::new(1, 2), UnitKey::new(1, 3)],
        2_000,
    );
    assert!(failed.is_err(), "越界根路径必须让整批失败: {failed:?}");
    assert_eq!(
        file_rows(&db, &good.file_id),
        0,
        "同事务前一个文件必须一起回滚"
    );
    assert_eq!(
        summary_tuple(&db, &library.id, &media_id),
        summary_before,
        "回滚后摘要不得前进"
    );

    drop(store);
    let reopened = Store::open(&data).unwrap();
    assert_eq!(
        reopened.recover_orphaned_scans().unwrap(),
        1,
        "只有 running 的那条会被重新排队"
    );
    assert_eq!(scan_status(&db, &interrupted.id), "queued");
    assert_eq!(
        scan_status(&db, &cancelled.id),
        "cancelled",
        "取消是终态，重启不得复活"
    );
    assert_eq!(
        summary_tuple(&db, &library.id, &media_id),
        summary_before,
        "重启后摘要必须与重启前逐列一致"
    );

    let after_restart = watch.finish();
    // task-98 诊断：panic 只打印 map，这里把窗口内的差异行逐列 dump 出来（含 `rowid` 身份列）。
    println!("[diag:restart] after_restart={after_restart:?}");
    let _diag_restart = watch.print_changed_tables("restart");
    // 行级可判定事实：窗口内**恰好一行既有行**被原地改（不是新增、不是删除）。
    // 该行 = 上面 `set_scan_operation_status(.., Running)` 置为 running 的那条 `sig-b`；
    // 写入点 = `crates/store/src/scan_operations.rs:443`
    // `UPDATE scan_operations SET status = 'queued' WHERE id = ?1 AND status = 'running'`（单语句自动提交）。
    assert_eq!(
        after_restart.changed_tables,
        BTreeMap::from([(
            "scan_operations".to_string(),
            TableDelta {
                inserted: 0,
                updated: 1,
                deleted: 0,
            },
        )]),
        "这个窗口只允许 scan_operations 的那一行被原地改（running→queued）"
    );
    drop(reopened);
}

#[test]
fn write_scope_instrument_detects_row_changes() {
    let fixture = fixture("Instrument Series");
    let db = fixture.dir.path().join("data/library.db");
    let watch = DbWatch::open(&db);
    let conn = open_db(&db);
    let user_id = UserId::new().to_string();
    let media_id = fixture.series.id.to_string();
    let row = rusqlite::params![user_id, media_id];

    conn.execute(
        "INSERT INTO playback_units
           (user_id, media_id, season, episode, played, has_played_assertion,
            favorite, play_count, watch_activity_at, position_ms)
         VALUES (?1, ?2, 1, 1, 0, 0, 0, 0, 9_999, 0)",
        row,
    )
    .unwrap();

    let facts = watch.finish();
    assert_eq!(
        facts.changed_tables.get("playback_units").copied(),
        Some(TableDelta {
            inserted: 1,
            updated: 0,
            deleted: 0,
        }),
        "正控制①：一行 INSERT 必须被数成 inserted=1: {facts:?}"
    );
    assert_eq!(facts.write_scope, 1, "窗口内只应有这一行变化: {facts:?}");
    assert_eq!(
        watch.library_changed_rows(&fixture.unowned_library),
        BTreeMap::new(),
        "这一行不属于那个 Library，不得被记到它头上"
    );

    // 正控制②：**同一行原地 UPDATE**。行级语义记 updated=1；旧的双向镜像计数会记 2，
    // 与「两次 INSERT」不可区分——这正是 task-98 要钉死的仪器歧义。
    watch.snapshot().unwrap();
    let touched = conn
        .execute(
            "UPDATE playback_units SET play_count = play_count + 1
             WHERE user_id = ?1 AND media_id = ?2 AND season = 1 AND episode = 1",
            row,
        )
        .unwrap();
    assert_eq!(touched, 1, "正控制必须恰好改到一行");
    let facts = watch.finish();
    assert_eq!(
        facts.changed_tables.get("playback_units").copied(),
        Some(TableDelta {
            inserted: 0,
            updated: 1,
            deleted: 0,
        }),
        "正控制②：一行原地 UPDATE 只记 1 行（不是前后镜像各记一次）: {facts:?}"
    );
    assert_eq!(
        facts.write_scope, 1,
        "一行 UPDATE 的写作用域是 1: {facts:?}"
    );

    // 正控制③：DELETE 记 deleted=1。
    watch.snapshot().unwrap();
    let touched = conn
        .execute(
            "DELETE FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = 1 AND episode = 1",
            row,
        )
        .unwrap();
    assert_eq!(touched, 1, "正控制必须恰好删到一行");
    let facts = watch.finish();
    assert_eq!(
        facts.changed_tables.get("playback_units").copied(),
        Some(TableDelta {
            inserted: 0,
            updated: 0,
            deleted: 1,
        }),
        "正控制③：一行 DELETE 必须被数成 deleted=1: {facts:?}"
    );
    assert_eq!(
        facts.write_scope, 1,
        "一行 DELETE 的写作用域是 1: {facts:?}"
    );
}

fn seed_rollback_series(
    store: &Store,
    root: &std::path::Path,
) -> (store::Library, LibraryFile, PathBuf) {
    let library = store
        .create_library(
            MediaKind::Tv,
            "Rollback Series",
            &[root.join("series").to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let library_id: LibraryId = library.id.parse().unwrap();
    let (root_id, root_path) = root_of(store, &library.id);
    let series = media(MediaKind::Tv, "Rollback Series");
    store.insert_media(&series).unwrap();
    let template = file_template(library_id, &root_id, series.id, MediaKind::Tv);
    let episode_one = episode_file(&template, &root_path, 1, 1);
    publish(
        store,
        &library_id,
        &[episode_one],
        &[UnitKey::new(1, 1)],
        1_000,
    )
    .unwrap();

    (library, template, root_path)
}
