//! 度量仪器与工作负载驱动：逐表行内容快照（写作用域）、现有公开读计数器、扫描批次驱动、
//! 扫描期间的心跳压测、以及 Step 2 的实测输出。这里不含断言。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use api::{ApiState, Store};
use axum::Router;
use domain::{LibraryFile, LibraryId, MediaId, UnitKey};
use parking_lot::Mutex;
use tower::ServiceExt;

use crate::fixtures::{TOKEN, episode_file, heartbeat_body, publish};

/// 一张表在一个窗口里的行级变化：按 `rowid` 认行身份，区分 INSERT / UPDATE / DELETE。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TableDelta {
    /// 窗口内新出现的行（rowid 不在快照里）。
    pub(crate) inserted: i64,
    /// rowid 两边都在、行内容变了：原地改的行。**一行只记 1。**
    pub(crate) updated: i64,
    /// 窗口内消失的行（rowid 只在快照里）。
    pub(crate) deleted: i64,
}

impl TableDelta {
    /// 行级写作用域：被写到的**不同行**数。一行原地 UPDATE 记 1，不记 2。
    pub(crate) fn rows(&self) -> i64 {
        self.inserted + self.updated + self.deleted
    }

    fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug)]
pub(crate) struct ScopedFacts {
    /// 恒为 `None`（未测得）；见 [`crate::READ_SCOPE_UNAVAILABLE`]。
    pub(crate) read_scope: Option<usize>,
    /// 窗口内 `library.db` 内容变化的**不同行**总数（行级语义，见 [`TableDelta`]）。
    pub(crate) write_scope: i64,
    /// 按表拆开的行级变化，防止双向抵消看着像「没写」。
    pub(crate) changed_tables: BTreeMap<String, TableDelta>,
}

/// 行级身份比对（task-98 仪器升级）。
///
/// 旧的双向 `EXCEPT` 计数把「一行原地 UPDATE」按前后镜像各记一次 ⇒ 1 行变化报 2，
/// 与「两次 INSERT」不可区分（Lead 裁定 ㊺：镜像计数是歧义的）。这里改成按行身份认行：
/// `INSERT` = identity 只在窗口后；`DELETE` = identity 只在快照里；
/// `UPDATE` = identity 两边都在但行内容不同。
///
/// identity 是 `rowid`，或在 layout v3 的 `WITHOUT ROWID` 窄复合主键表上用主键元组
/// （见 [`DbWatch::identity`]）：两种形状都满足「同一行前后相同、不同行不同」。
///
/// 返回 4 个数 `(镜像(后), 新增, 删除, 镜像(前))`；恒等式
/// `镜像(后) = INSERT + UPDATE`、`镜像(前) = DELETE + UPDATE`（见 [`DbWatch::row_delta`]）。
fn row_delta_sql(identity: &str, table: &str, index: usize, scope: &str) -> String {
    format!(
        "SELECT (SELECT COUNT(*) FROM (SELECT {identity} AS scope_rowid, * FROM \"{table}\"{scope} \
         EXCEPT SELECT * FROM scope_snapshot_{index}{scope})), \
         (SELECT COUNT(*) FROM (SELECT {identity} FROM \"{table}\"{scope} \
         EXCEPT SELECT scope_rowid FROM scope_snapshot_{index}{scope})), \
         (SELECT COUNT(*) FROM (SELECT scope_rowid FROM scope_snapshot_{index}{scope} \
         EXCEPT SELECT {identity} FROM \"{table}\"{scope})), \
         (SELECT COUNT(*) FROM (SELECT * FROM scope_snapshot_{index}{scope} \
         EXCEPT SELECT {identity} AS scope_rowid, * FROM \"{table}\"{scope}))"
    )
}

/// 观测量仪：自带一条到 `library.db` 的连接，逐表做前后内容快照。
pub(crate) struct DbWatch {
    conn: rusqlite::Connection,
    tables: Vec<String>,
}

impl DbWatch {
    pub(crate) fn open(db: &Path) -> Self {
        let conn = open_db(db);
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        drop(stmt);
        let watch = Self { conn, tables };
        watch.snapshot().unwrap();
        watch
    }

    /// 重新快照，作为下一个观测窗口的基线。
    ///
    /// 快照**保留行身份**（额外一列 `scope_rowid`）——行身份靠它，不靠全列内容相等。
    pub(crate) fn snapshot(&self) -> rusqlite::Result<()> {
        for (index, table) in self.tables.iter().enumerate() {
            let identity = self.identity(table);
            self.conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS temp.scope_snapshot_{index};
                 CREATE TEMP TABLE scope_snapshot_{index} AS
                 SELECT {identity} AS scope_rowid, * FROM \"{table}\";"
            ))?;
        }
        Ok(())
    }

    /// 行身份表达式：`rowid` 表就是 `rowid`；`WITHOUT ROWID` 表（layout v3 的窄复合
    /// 主键表，例如 `library_item_terms` / `user_library_items`）没有 `rowid`，改用主键
    /// 元组。两者都是「同一行前后相同、不同行不同」，所以 ㊺ 的行级语义不变。
    fn identity(&self, table: &str) -> String {
        let without_rowid: i64 = self
            .conn
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        if without_rowid == 0 {
            return "rowid".to_string();
        }
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
            .unwrap();
        let columns: Vec<String> = stmt
            .query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .map(|column| format!("quote(\"{column}\")"))
            .collect();
        columns.join("||char(31)||")
    }

    /// 表级行级变化量；`library_id` 非空时只统计该 Library 的行。
    fn row_delta(
        &self,
        table: &str,
        index: usize,
        scope: &str,
        library_id: Option<&str>,
    ) -> TableDelta {
        let sql = row_delta_sql(&self.identity(table), table, index, scope);
        let read = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        };
        let (mirror_after, inserted, deleted, mirror_before) = match library_id {
            Some(id) => self.conn.query_row(&sql, [id], read).unwrap(),
            None => self.conn.query_row(&sql, [], read).unwrap(),
        };
        let updated = mirror_after - inserted;
        debug_assert_eq!(
            mirror_before - deleted,
            updated,
            "表 {table} 行级比对自相矛盾：镜像(前)={mirror_before} 删除={deleted}"
        );
        TableDelta {
            inserted,
            updated,
            deleted,
        }
    }

    fn changed_rows(&self) -> BTreeMap<String, TableDelta> {
        let mut changed = BTreeMap::new();
        for (index, table) in self.tables.iter().enumerate() {
            let delta = self.row_delta(table, index, "", None);
            if !delta.is_zero() {
                changed.insert(table.clone(), delta);
            }
        }
        changed
    }

    /// 只看某一个 Library 的行：用于「不拥有该单元的 Library 未被写」。
    ///
    /// 凡是有 `library_id` 列的表都参与（`libraries` / `library_roots` / `scan_operations` 也算），
    /// 不依赖任何「影响行数」声明，只看行内容真没变。
    pub(crate) fn library_changed_rows(&self, library_id: &str) -> BTreeMap<String, TableDelta> {
        let mut changed = BTreeMap::new();
        for (index, table) in self.tables.iter().enumerate() {
            if !self.has_column(table, "library_id") {
                continue;
            }
            let delta = self.row_delta(table, index, " WHERE library_id = ?1", Some(library_id));
            if !delta.is_zero() {
                changed.insert(table.clone(), delta);
            }
        }
        changed
    }

    /// 诊断用（task-98）：逐列打印某表在最近一个窗口里的「窗口后独有行」与「快照独有行」。
    ///
    /// 两侧都带行身份列（快照侧叫 `scope_rowid`），列数必须一致才能 `EXCEPT`；
    /// 两行 dump 的 `scope_rowid` 相同即证明是**同一行被原地改**，不是一删一增。
    ///
    /// 只在测试里调用（`--nocapture` 可见）；不参与任何断言，也不写生产日志。
    pub(crate) fn print_diff_rows(&self, table: &str) {
        let Some(index) = self.tables.iter().position(|name| name == table) else {
            println!("[diag] 表 {table} 不在快照清单里");
            return;
        };
        let identity = self.identity(table);
        for (label, sql) in [
            (
                "窗口后独有(新增/改后)",
                format!(
                    "SELECT {identity} AS scope_rowid, * FROM \"{table}\" EXCEPT \
                     SELECT * FROM scope_snapshot_{index}"
                ),
            ),
            (
                "快照独有(删除/改前)",
                format!(
                    "SELECT * FROM scope_snapshot_{index} EXCEPT \
                     SELECT {identity} AS scope_rowid, * FROM \"{table}\""
                ),
            ),
        ] {
            let mut stmt = self.conn.prepare(&sql).unwrap();
            let columns = stmt.column_count();
            let names: Vec<String> = stmt
                .column_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            let mut rows = stmt.query([]).unwrap();
            let mut count = 0usize;
            while let Some(row) = rows.next().unwrap() {
                count += 1;
                let cells: Vec<String> = (0..columns)
                    .map(|position| format!("{:?}", row.get_ref(position).unwrap()))
                    .collect();
                println!(
                    "[diag:{table}] {label} #{count} [{}] {}",
                    names.join(","),
                    cells.join(" | ")
                );
            }
            if count == 0 {
                println!("[diag:{table}] {label}: 0 行");
            }
        }
    }

    /// 诊断用（task-98）：打印窗口内的逐表行级变化量，并把每张变化表的差异行逐列 dump。
    pub(crate) fn print_changed_tables(&self, label: &str) -> BTreeMap<String, TableDelta> {
        let changed = self.changed_rows();
        println!(
            "[diag:{label}] 行级 write_scope={} changed_tables={changed:?}",
            changed.values().map(TableDelta::rows).sum::<i64>()
        );
        for table in changed.keys() {
            self.print_diff_rows(table);
        }
        changed
    }

    fn has_column(&self, table: &str, column: &str) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                rusqlite::params![table, column],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    pub(crate) fn finish(&self) -> ScopedFacts {
        let changed_tables = self.changed_rows();
        ScopedFacts {
            read_scope: None,
            write_scope: changed_tables.values().map(TableDelta::rows).sum(),
            changed_tables,
        }
    }
}

/// 现有的公开读计数器：**粗粒度**读数指标，明确**不是** `read_scope`。
pub(crate) fn read_counters(state: &ApiState) -> (usize, usize, usize) {
    let binding = state.store();
    let store = binding.lock();
    (
        store.root_ledger_scans(),
        store.media_ledger_reads(),
        store.role_lookups(),
    )
}

/// 去程差：`read_counters` 是**累积**计数器，比较两个窗口必须用「窗口内增量」，
/// 直接把含一次心跳的累积值当不变量比较是结构性错误（task-98 诊断）。
pub(crate) fn counter_delta(
    after: (usize, usize, usize),
    before: (usize, usize, usize),
) -> (usize, usize, usize) {
    (
        after.0.saturating_sub(before.0),
        after.1.saturating_sub(before.1),
        after.2.saturating_sub(before.2),
    )
}

pub(crate) fn open_db(db: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db).unwrap();
    // 与 Store 的连接策略对齐，避免外部连接自己变成新的锁来源。
    conn.busy_timeout(Duration::from_millis(5_000)).unwrap();
    conn
}

pub(crate) fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// 进程 RSS；`ps` 不可用时返回 `None`（报告里写「未测得」，不编数）。
pub(crate) fn rss_kb() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", pid.as_str()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn percentile(sorted: &[u128], percent: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * percent) / 100]
}

pub(crate) struct ScanPlan {
    pub(crate) library_id: LibraryId,
    pub(crate) template: LibraryFile,
    pub(crate) root: PathBuf,
    pub(crate) first_episode: i32,
    pub(crate) batches: usize,
    pub(crate) per_batch: usize,
}

#[derive(Debug)]
pub(crate) struct ScanReport {
    pub(crate) inserted: usize,
    pub(crate) updated: usize,
    pub(crate) unchanged: usize,
    pub(crate) batch_ms: Vec<u128>,
    pub(crate) errors: Vec<String>,
}

/// 扫描侧：拿真实 `publish_files`（每批 ≤100 文件 ⇒ 一个短事务）连续写一整个 Media。
pub(crate) fn run_scan(store: Arc<Mutex<Store>>, plan: ScanPlan) -> ScanReport {
    let mut report = ScanReport {
        inserted: 0,
        updated: 0,
        unchanged: 0,
        batch_ms: Vec::new(),
        errors: Vec::new(),
    };
    for batch in 0..plan.batches {
        let mut files = Vec::with_capacity(plan.per_batch);
        let mut keys = Vec::with_capacity(plan.per_batch);
        for index in 0..plan.per_batch {
            let episode = plan.first_episode + (batch * plan.per_batch + index) as i32;
            files.push(episode_file(&plan.template, &plan.root, 1, episode));
            keys.push(UnitKey::new(1, episode));
        }
        let started = Instant::now();
        let outcome = {
            let guard = store.lock();
            publish(
                &guard,
                &plan.library_id,
                &files,
                &keys,
                2_000 + batch as i64,
            )
        };
        report.batch_ms.push(started.elapsed().as_millis());
        match outcome {
            Ok(counts) => {
                report.inserted += counts.inserted;
                report.updated += counts.updated;
                report.unchanged += counts.unchanged;
            }
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    report
}

/// 扫描进行中连续打心跳，返回 (每次端到端延迟, 状态码分布)。
pub(crate) async fn hammer_while_scanning(
    app: &Router,
    series: MediaId,
    device: &str,
    session: &str,
    scanner: &JoinHandle<ScanReport>,
) -> (Vec<Duration>, BTreeMap<u16, usize>) {
    let mut latencies = Vec::new();
    let mut statuses: BTreeMap<u16, usize> = BTreeMap::new();
    let mut failure_codes: BTreeMap<String, usize> = BTreeMap::new();
    while (!scanner.is_finished() || latencies.len() < 20) && latencies.len() < 5_000 {
        let position = 31_000 + (latencies.len() as i64 % 2_000) * 1_000;
        let started = Instant::now();
        let response = app
            .clone()
            .oneshot(crate::common::request(
                "POST",
                "/api/v1/playback/progress",
                Some(TOKEN),
                heartbeat_body(series, position, device, session),
            ))
            .await
            .unwrap();
        latencies.push(started.elapsed());
        let status = response.status().as_u16();
        *statuses.entry(status).or_default() += 1;
        if !(200..300).contains(&status) {
            // 非 2xx 只记状态码不足以定位：把信封里的 code/message 一起打出来，
            // 才能区分 auth / handler 读 / handler 写 / 门禁 哪一段判的繁忙。
            let body = crate::common::json_body(response).await;
            let elapsed = latencies.last().copied().unwrap_or_default();
            *failure_codes
                .entry(format!(
                    "{status} {} {} @{elapsed:?}",
                    body["error"]["code"], body["error"]["message"]
                ))
                .or_default() += 1;
        }
    }
    if !failure_codes.is_empty() {
        println!("非 2xx 明细（状态 + code + message → 次数）: {failure_codes:?}");
    }
    (latencies, statuses)
}

/// Step 2 的实测输出：吞吐 / 单批提交耗时 / 心跳延迟 / busy / WAL / RSS。取不到的写「未测得」。
pub(crate) fn print_step_two(
    report: &ScanReport,
    latencies: &[Duration],
    statuses: &BTreeMap<u16, usize>,
    wal: (u64, u64),
    rss: (Option<u64>, Option<u64>),
    per_batch: usize,
) {
    let mut batch_ms = report.batch_ms.clone();
    let mut heartbeat_ms: Vec<u128> = latencies.iter().map(Duration::as_millis).collect();
    batch_ms.sort_unstable();
    heartbeat_ms.sort_unstable();
    let scan_total_ms: u128 = report.batch_ms.iter().sum();
    let episodes = report.inserted + report.updated + report.unchanged;
    let throughput = episodes as f64 * 1000.0 / scan_total_ms.max(1) as f64;
    let (mut busy, mut non_2xx): (usize, Vec<u16>) = (0, Vec::new());
    for (code, count) in statuses {
        if *code == 503 || *code == 429 {
            busy += count;
        } else if !(200..300).contains(code) {
            non_2xx.push(*code);
        }
    }
    println!("--- Step 2 实测（--release，本机，单进程） ---");
    println!(
        "扫描: {} 批 × {per_batch} 文件 = {episodes} 集，实测总时长 {scan_total_ms} ms，吞吐 {throughput:.1} 文件/秒",
        report.batch_ms.len()
    );
    println!(
        "单批提交（含等待持锁）: p50={} ms, p95={} ms, max={} ms（最长事务看这里的 max）",
        percentile(&batch_ms, 50),
        percentile(&batch_ms, 95),
        percentile(&batch_ms, 100)
    );
    println!(
        "并发心跳 {} 次，端到端延迟: p50={} ms, p95={} ms, max={} ms",
        latencies.len(),
        percentile(&heartbeat_ms, 50),
        percentile(&heartbeat_ms, 95),
        percentile(&heartbeat_ms, 100)
    );
    println!("心跳状态码分布: {statuses:?}；busy(503/429)={busy}；非 2xx={non_2xx:?}");
    println!("扫描错误: {:?}", report.errors);
    println!(
        "WAL: {} -> {} 字节（增长 {}）",
        wal.0,
        wal.1,
        wal.1 as i64 - wal.0 as i64
    );
    match rss {
        (Some(before), Some(after)) => {
            println!(
                "进程 RSS: {} -> {} KiB（增长 {}）",
                before,
                after,
                after as i64 - before as i64
            )
        }
        _ => println!("进程 RSS: 未测得（`ps -o rss= -p <pid>` 不可用）"),
    }
    println!(
        "锁等待: 未测得（无公开计数器）；上面的 max 是「事务时长 + 排队」的上界，不是纯锁等待"
    );
}
