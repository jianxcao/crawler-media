//! DB8 Task 4：数据库维护目标（allowlist）与**有界** SQLite 执行器。
//!
//! 设计约束（方案第 3.2 / 5.2 节）：
//!
//! - 路径只由内部事实产生：Store 真实 `app_path` + `data_dir` 下的固定文件名，
//!   外加**已存在**的可选 fanart 库。不接受任意用户路径，也不会创建空数据库。
//! - 借用该库唯一 writer 连接，短 `busy_timeout=250ms`，退出时恢复原策略；不为维护重复
//!   迁移/seed，不执行 `wal_checkpoint(TRUNCATE)`，不删除/搬移 DB/WAL/SHM 文件。
//! - 轻量路径只有 `PRAGMA optimize`；VACUUM 需要显式开启 + 冷却 + 空闲页阈值 +
//!   磁盘准入，全部通过才执行。
//! - 每库有界执行：`InterruptHandle` + 看门狗线程，超时 `interrupt()` 并把该库标成
//!   `Failed`；正常完成时通过 channel 通知看门狗提前退出并 `join`，不留残留线程。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ffi};

use super::policy::{
    VACUUM_MIN_FREE_BYTES, VACUUM_MIN_FREE_RATIO_PERCENT, VACUUM_MIN_INTERVAL_SECS,
};
use super::report::{DatabaseOutcome, DatabaseReport};

/// 单库执行上限。超时走 SQLite interrupt，不用 Tokio timeout 假装已停止。
pub const PER_DATABASE_LIMIT: Duration = Duration::from_secs(30);

/// VACUUM 前的额外磁盘预留（方案第 5.2 节的 64MiB）。
pub const VACUUM_DISK_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

/// 维护连接的 `busy_timeout`：不沿用前台 5s，避免维护积压。
const MAINTENANCE_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// 超时后重试中断的节奏。
///
/// 实测（rusqlite 0.37 + bundled SQLite）：
///
/// - `sqlite3_interrupt` 落在两条语句之间会被**丢弃**（SQLite 在一步结束时、
///   没有活跃 VDBE 时清标志），单次中断可能完全无效；
/// - VACUUM 的页拷贝阶段走 `sqlite3_backup`，**不检查** interrupt 标志。
///
/// 因此到期后以很短的节奏反复中断，直到主线程通知完成：落在可中断语句（`PRAGMA
/// optimize`、VACUUM 的拷贝前阶段）上的中断会让该库以 `SQLITE_INTERRUPT` 结束；
/// 若整段超时都落在页拷贝里，SQLite 无法停止，只能如实记录 `Vacuumed` 与超限的
/// `elapsed_ms`（见 DB8 报告"偏离"一节）。
const INTERRUPT_RETRY: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseName {
    App,
    Cache,
    Library,
    Subscribe,
    Jobs,
}

impl DatabaseName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Cache => "cache",
            Self::Library => "library",
            Self::Subscribe => "subscribe",
            Self::Jobs => "jobs",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DatabaseTarget {
    pub name: DatabaseName,
    pub path: PathBuf,
    /// Filesystem observations captured before acquiring any database writer.
    pub size_before: u64,
    pub available_disk_bytes: Option<u64>,
}

impl DatabaseTarget {
    fn observe(name: DatabaseName, path: PathBuf) -> Self {
        Self {
            name,
            size_before: file_len(&path),
            available_disk_bytes: available_disk_bytes(&path),
            path,
        }
    }

    pub(crate) fn finish_report(&self, report: &mut DatabaseReport) {
        report.size_after = file_len(&self.path);
    }
}

/// 可选库：文件不存在时记 Skipped，**不**创建。
const OPTIONAL: [DatabaseName; 0] = [];

fn data_dir_path(name: DatabaseName, data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{}.db", name.as_str()))
}

/// 维护目标 allowlist：App 用 Store 的真实 `app_path`，其余是 `data_dir` 下的固定
/// 文件名；可选 fanart 仅当文件已存在才进入列表。
pub fn database_targets(data_dir: &Path, app_path: &Path) -> Vec<DatabaseTarget> {
    let mut targets = vec![
        DatabaseTarget::observe(DatabaseName::App, app_path.to_path_buf()),
        DatabaseTarget::observe(
            DatabaseName::Cache,
            data_dir_path(DatabaseName::Cache, data_dir),
        ),
        DatabaseTarget::observe(
            DatabaseName::Library,
            data_dir_path(DatabaseName::Library, data_dir),
        ),
        DatabaseTarget::observe(
            DatabaseName::Subscribe,
            data_dir_path(DatabaseName::Subscribe, data_dir),
        ),
        DatabaseTarget::observe(
            DatabaseName::Jobs,
            data_dir_path(DatabaseName::Jobs, data_dir),
        ),
    ];
    for name in OPTIONAL {
        let path = data_dir_path(name, data_dir);
        if path.is_file() {
            targets.push(DatabaseTarget::observe(name, path));
        }
    }
    targets
}

/// 声明过但当前不存在的可选库：报告里记 Skipped，绝不创建文件。
pub(crate) fn missing_optional_targets(data_dir: &Path) -> Vec<DatabaseTarget> {
    OPTIONAL
        .iter()
        .map(|name| DatabaseTarget::observe(*name, data_dir_path(*name, data_dir)))
        .filter(|target| !target.path.is_file())
        .collect()
}

/// 一次执行的只读上下文。
pub struct MaintenanceContext<'a> {
    pub now: i64,
    pub last_vacuum_success: Option<i64>,
    /// 读不到上次成功时间时为 true。不能当成“从未 VACUUM”而缩短冷却。
    pub vacuum_history_unreadable: bool,
    pub cancelled: &'a dyn Fn() -> bool,
}

pub trait DatabaseExecutor: Send + Sync {
    fn execute(
        &self,
        conn: &Connection,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String>;
}

/// 真实 SQLite 执行器的阈值与超时。生产用 policy 常量；测试可缩小/替换阈值，
/// 在小库上走通 VACUUM 与中断路径。
#[derive(Clone, Copy, Debug)]
pub struct SqliteLimits {
    pub min_free_bytes: u64,
    pub min_free_ratio_percent: u32,
    pub vacuum_headroom_bytes: u64,
    pub per_database_limit: Duration,
}

impl SqliteLimits {
    pub fn production() -> Self {
        Self {
            min_free_bytes: VACUUM_MIN_FREE_BYTES,
            min_free_ratio_percent: VACUUM_MIN_FREE_RATIO_PERCENT,
            vacuum_headroom_bytes: VACUUM_DISK_HEADROOM_BYTES,
            per_database_limit: PER_DATABASE_LIMIT,
        }
    }
}

impl Default for SqliteLimits {
    fn default() -> Self {
        Self::production()
    }
}

/// 生产执行器：policy 常量阈值 + 每库 30s 上限。
pub struct SqliteExecutor;

/// 同一套真实逻辑，但使用注入的阈值/超时（测试与诊断用）。
pub struct ConfiguredSqliteExecutor {
    limits: SqliteLimits,
}

impl ConfiguredSqliteExecutor {
    pub fn with_limits(limits: SqliteLimits) -> Self {
        Self { limits }
    }
}

impl DatabaseExecutor for SqliteExecutor {
    fn execute(
        &self,
        conn: &Connection,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        run_sqlite(
            conn,
            target,
            vacuum_enabled,
            context,
            SqliteLimits::production(),
        )
    }
}

impl DatabaseExecutor for ConfiguredSqliteExecutor {
    fn execute(
        &self,
        conn: &Connection,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        run_sqlite(conn, target, vacuum_enabled, context, self.limits)
    }
}

/// Maintain the already acquired unique writer. All filesystem observations belong
/// to the caller, before admission and after this borrowed connection is released.
fn run_sqlite(
    conn: &Connection,
    target: &DatabaseTarget,
    vacuum_enabled: bool,
    context: &MaintenanceContext<'_>,
    limits: SqliteLimits,
) -> Result<DatabaseReport, String> {
    let started = Instant::now();
    let size_before = target.size_before;
    let _policy = BusyPolicy::install(conn)?;

    let interrupt = conn.get_interrupt_handle();
    let (done_tx, done_rx) = channel::<()>();
    let limit = limits.per_database_limit;
    let watchdog = std::thread::spawn(move || {
        let mut wait = limit;
        loop {
            match done_rx.recv_timeout(wait) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    interrupt.interrupt();
                    wait = INTERRUPT_RETRY;
                }
            }
        }
    });

    let work = execute_work(conn, target, vacuum_enabled, context, limits, size_before);
    // 正常完成（或被中断返回）后通知看门狗提前退出并 join，避免残留线程在连接释放后
    // 仍持有 InterruptHandle。
    let _ = done_tx.send(());
    let _ = watchdog.join();

    Ok(DatabaseReport {
        name: target.name.as_str().to_string(),
        outcome: work.outcome,
        free_pages: work.free_pages,
        page_size: work.page_size,
        size_before,
        size_after: size_before,
        skip_reason: work.skip_reason,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// A maintenance attempt must never leave the shared foreground connection with
/// its shortened SQLite busy policy. Progress handlers are left untouched.
struct BusyPolicy<'a> {
    conn: &'a Connection,
    previous: Duration,
}

impl<'a> BusyPolicy<'a> {
    fn install(conn: &'a Connection) -> Result<Self, String> {
        let millis: u64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .map_err(|error| format!("读取数据库 busy_timeout 失败: {error}"))?;
        let policy = Self {
            conn,
            previous: Duration::from_millis(millis),
        };
        conn.busy_timeout(MAINTENANCE_BUSY_TIMEOUT)
            .map_err(|error| format!("设置数据库维护 busy_timeout 失败: {error}"))?;
        Ok(policy)
    }
}

impl Drop for BusyPolicy<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.conn.busy_timeout(self.previous) {
            tracing::error!(%error, "恢复数据库维护前的 busy_timeout 失败");
        }
    }
}

struct WorkOutcome {
    outcome: DatabaseOutcome,
    skip_reason: Option<String>,
    free_pages: i64,
    page_size: i64,
}

fn execute_work(
    conn: &Connection,
    target: &DatabaseTarget,
    vacuum_enabled: bool,
    context: &MaintenanceContext<'_>,
    limits: SqliteLimits,
    size_before: u64,
) -> WorkOutcome {
    if let Err(error) = conn.execute_batch("PRAGMA optimize;") {
        return classify(&error, "PRAGMA optimize");
    }
    let page_size = pragma_i64(conn, "page_size");
    let page_count = pragma_i64(conn, "page_count");
    let free_pages = pragma_i64(conn, "freelist_count");
    let light = WorkOutcome {
        outcome: DatabaseOutcome::Optimized,
        skip_reason: None,
        free_pages,
        page_size,
    };
    if !vacuum_enabled {
        // 轻量路径不重建文件，但仍然回收空闲页尾部（`incremental_vacuum` 不拿独占锁）。
        log_incremental_reclaim(conn, target, free_pages);
        return light;
    }
    if (context.cancelled)() {
        return skip(
            "已执行 PRAGMA optimize；VACUUM 跳过：任务已取消",
            free_pages,
            page_size,
        );
    }
    let facts = VacuumFacts {
        free_pages: as_u64(free_pages),
        page_size: as_u64(page_size),
        page_count: as_u64(page_count),
        now: context.now,
        last_vacuum_success: context.last_vacuum_success,
        vacuum_history_unreadable: context.vacuum_history_unreadable,
        limits,
    };
    // 完整 VACUUM 的准入窗口：冷却、空闲页字节/比例、磁盘余量全部满足才重建文件。
    // 没进窗口时改跑 `PRAGMA incremental_vacuum(N)`，并在报告里写明为什么没升级到
    // 完整 VACUUM、增量回收了多少页（或该库为何不支持增量回收）。
    if let Some(reason) = vacuum_skip_reason(&facts) {
        return skip_after_incremental(conn, reason, free_pages, page_size);
    }
    if let Some(reason) = disk_skip_reason(target, size_before, limits) {
        return skip_after_incremental(conn, reason, free_pages, page_size);
    }
    match conn.execute_batch("VACUUM;") {
        Ok(()) => WorkOutcome {
            outcome: DatabaseOutcome::Vacuumed,
            skip_reason: None,
            free_pages,
            page_size,
        },
        Err(error) => classify(&error, "VACUUM"),
    }
}

/// VACUUM 准入事实（全部整数，避免溢出/除零/浮点误差）。
struct VacuumFacts {
    free_pages: u64,
    page_size: u64,
    page_count: u64,
    now: i64,
    last_vacuum_success: Option<i64>,
    vacuum_history_unreadable: bool,
    limits: SqliteLimits,
}

/// 冷却与空闲页阈值。`Some(reason)` 表示必须跳过并写明哪一条不满足。
fn vacuum_skip_reason(facts: &VacuumFacts) -> Option<String> {
    if facts.vacuum_history_unreadable {
        return Some("读不到上次成功 VACUUM 时间，保守跳过本轮 VACUUM".into());
    }
    if let Some(last) = facts.last_vacuum_success {
        let elapsed = u64::try_from(facts.now.saturating_sub(last)).unwrap_or(0);
        if elapsed < VACUUM_MIN_INTERVAL_SECS {
            return Some(format!(
                "冷却期未到（距上次成功 VACUUM {elapsed} 秒，需要 {VACUUM_MIN_INTERVAL_SECS} 秒）"
            ));
        }
    }
    let free_bytes = facts.free_pages.saturating_mul(facts.page_size);
    if free_bytes < facts.limits.min_free_bytes {
        return Some(format!(
            "空闲页 {free_bytes} 字节 < 阈值 {} 字节",
            facts.limits.min_free_bytes
        ));
    }
    let ratio = facts
        .free_pages
        .saturating_mul(100)
        .checked_div(facts.page_count)
        .unwrap_or(0);
    if ratio < u64::from(facts.limits.min_free_ratio_percent) {
        return Some(format!(
            "空闲页比例 {ratio}% < 阈值 {}%",
            facts.limits.min_free_ratio_percent
        ));
    }
    None
}

/// 磁盘准入：保守准入，不承诺执行中空间不变；拿不到可用空间时也跳过。
fn disk_skip_reason(
    target: &DatabaseTarget,
    size_before: u64,
    limits: SqliteLimits,
) -> Option<String> {
    let required = size_before
        .saturating_mul(2)
        .saturating_add(limits.vacuum_headroom_bytes);
    match target.available_disk_bytes {
        Some(available) if available >= required => None,
        Some(available) => Some(format!(
            "已执行 PRAGMA optimize；VACUUM 跳过：磁盘可用空间 {available} 字节 < 需要 {required} 字节（2 × 主体 + 预留）"
        )),
        None => Some("已执行 PRAGMA optimize；VACUUM 跳过：无法确定可用磁盘空间".to_string()),
    }
}

fn skip(reason: impl Into<String>, free_pages: i64, page_size: i64) -> WorkOutcome {
    WorkOutcome {
        outcome: DatabaseOutcome::Skipped,
        skip_reason: Some(reason.into()),
        free_pages,
        page_size,
    }
}

/// `PRAGMA auto_vacuum` 的取值：0 = 关闭，1 = 全量，2 = incremental。
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// 回收空闲页但不重建文件：`PRAGMA incremental_vacuum(N)`。
///
/// 只有以 `auto_vacuum=INCREMENTAL` 建过的库才有意义（建库时的 pragma 由
/// `open.rs` 的 create 路径负责；`jobs.db` 由 `jobs::Queue` 打开，没有走那条路径，
/// 因此这里必须**先读** `PRAGMA auto_vacuum`，不支持就如实返回原因，不假装回收过。
fn reclaim_incremental(conn: &Connection, free_pages: i64) -> Result<i64, String> {
    if free_pages <= 0 {
        return Ok(0);
    }
    let mode = pragma_i64(conn, "auto_vacuum");
    if mode != AUTO_VACUUM_INCREMENTAL {
        return Err(format!(
            "该库 auto_vacuum={mode}（非 incremental），增量回收不适用"
        ));
    }
    conn.execute_batch(&format!("PRAGMA incremental_vacuum({free_pages});"))
        .map_err(|error| format!("PRAGMA incremental_vacuum({free_pages}) 失败: {error}"))?;
    Ok(free_pages)
}

/// 轻量路径的增量回收只进日志：`skip_reason` 保持为空表示"没有跳过任何东西"。
fn log_incremental_reclaim(conn: &Connection, target: &DatabaseTarget, free_pages: i64) {
    match reclaim_incremental(conn, free_pages) {
        Ok(0) => {}
        Ok(reclaimed) => tracing::debug!(
            database = target.name.as_str(),
            reclaimed,
            "已增量回收空闲页"
        ),
        Err(reason) => tracing::debug!(database = target.name.as_str(), %reason),
    }
}

/// 完整 VACUUM 未准入时：先尽力增量回收，再把两条事实一起写进 `skip_reason`。
fn skip_after_incremental(
    conn: &Connection,
    reason: String,
    free_pages: i64,
    page_size: i64,
) -> WorkOutcome {
    let note = match reclaim_incremental(conn, free_pages) {
        Ok(0) => format!("{reason}；没有空闲页需要回收"),
        Ok(reclaimed) => {
            format!("{reason}；已执行 PRAGMA incremental_vacuum 回收 {reclaimed} 页")
        }
        Err(why) => format!("{reason}；增量回收未执行：{why}"),
    };
    skip(note, free_pages, page_size)
}

/// busy/locked → 跳过（下轮再试）；其余（IO/corrupt/full/中断）→ 失败并保留上下文。
fn classify(error: &rusqlite::Error, step: &str) -> WorkOutcome {
    let text = format!("{step} 失败: {error}");
    let outcome = match error {
        rusqlite::Error::SqliteFailure(inner, _) => match inner.code {
            ffi::ErrorCode::DatabaseBusy | ffi::ErrorCode::DatabaseLocked => {
                return WorkOutcome {
                    outcome: DatabaseOutcome::Skipped,
                    skip_reason: Some(format!("{text}（数据库忙/被锁，下轮再试）")),
                    free_pages: 0,
                    page_size: 0,
                };
            }
            ffi::ErrorCode::OperationInterrupted => {
                return WorkOutcome {
                    outcome: DatabaseOutcome::Failed,
                    skip_reason: Some(format!("{text}（超过单库执行上限，已被中断）")),
                    free_pages: 0,
                    page_size: 0,
                };
            }
            _ => DatabaseOutcome::Failed,
        },
        _ => DatabaseOutcome::Failed,
    };
    WorkOutcome {
        outcome,
        skip_reason: Some(text),
        free_pages: 0,
        page_size: 0,
    }
}

fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    // 诊断读取：读不到按 0 处理，会让阈值判断保守地跳过 VACUUM。
    conn.pragma_query_value(None, name, |row| row.get(0))
        .unwrap_or(0)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[cfg(unix)]
fn available_disk_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs 只写它自己的 out 参数；raw 是合法的 NUL 结尾 C 字符串。
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(raw.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: 返回 0 表示 statvfs 已初始化 stats。
    let stats = unsafe { stats.assume_init() };
    let block = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    Some((stats.f_bavail as u64).saturating_mul(block))
}

#[cfg(not(unix))]
fn available_disk_bytes(_path: &Path) -> Option<u64> {
    None
}
