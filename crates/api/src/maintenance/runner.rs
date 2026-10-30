//! DB8 Task 4：维护任务 runner。
//!
//! 职责边界：
//!
//! - 按 `job.kind` 严格解析版本化策略；解析失败**直接失败**，绝不回退成更激进的策略。
//! - 固定本轮的 `now`（取 claim 时的 `started_at`）与 `cutoff`，避免执行期间窗口漂移。
//! - 每批只持一次短锁，批间释放；不在持 Store 锁时取 Queue 锁，反之亦然。
//! - Jobs 只读观察走统一读池；写入走 [`QueueGuard`]，runner 不继承 Queue 锁。
//! - 每轮结束把汇总报告写进 `job.progress`（成功与失败都写）。
//!
//! 数据库维护逐库借用唯一 writer；文件/磁盘观察在 writer 外进行。
//! 其他数据库与只读池继续服务，SQLite busy 与有界执行仍然必须存在。

use std::time::Instant;

use jobs::{Job, JobStatus};

use super::data_retention::run_data_retention;
use super::database::{
    DatabaseName, MaintenanceContext, database_targets, missing_optional_targets,
};
use super::policy::{
    BATCH_SIZE, DatabaseMaintenancePolicy, MAX_BATCH_BUDGET, MAX_BATCHES, MaintenancePolicy,
};
use super::report::{DatabaseOutcome, DatabaseReport, MaintenanceReport};
use crate::management::ApiState;

const DAY_SECS: i64 = 86_400;

/// 维护类任务的统一入口（`worker::StateRunner` 在**不持 Queue 锁**时调用）。
pub fn run(state: &ApiState, job: &Job) -> Result<(), String> {
    let policy = MaintenancePolicy::parse(job.kind, &job.payload)?;
    // 进程内互斥：`concurrency_key` 只防同一个 def 重叠，这里覆盖不同维护 def。
    let _exclusive = state
        .maintenance_lock
        .try_lock()
        .ok_or("已有维护任务在执行，本轮不重叠")?;

    let queue = QueueGuard::new(&state.jobs);
    let now = job.started_at.unwrap_or_else(crate::job_loop::unix_now);
    let started = Instant::now();
    let check_stop = || check_maintenance_stop(state, job);
    let version = match &policy {
        MaintenancePolicy::Database(policy) => policy.version,
        MaintenancePolicy::Jobs(policy) => policy.version,
        MaintenancePolicy::PlaybackHistory(policy) => policy.version,
        MaintenancePolicy::DataRetention(policy) => policy.version,
    };
    let mut report = MaintenanceReport::new(job.kind, version);
    tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "维护任务开始执行");

    let outcome = match &policy {
        MaintenancePolicy::Database(policy) => {
            run_database(state, job, policy, now, &check_stop, &mut report)
        }
        MaintenancePolicy::Jobs(policy) => {
            let cutoff = cutoff(now, policy.retention_days)?;
            report.cutoff = Some(cutoff);
            report.dry_run = policy.dry_run;
            let dry_run = policy.dry_run;
            // 游标按 job def 隔离（B3 §5.1）；没有 def 的临时任务共用一个键。
            let cursor_key = job
                .def_id
                .map(|def_id| def_id.to_string())
                .unwrap_or_else(|| "__adhoc__".to_string());
            run_batches(&mut report, dry_run, &check_stop, || {
                let window = if dry_run {
                    crate::jobs_readers::read(&state.jobs_readers, |queue| {
                        queue.preview_terminal_window(&cursor_key, cutoff, BATCH_SIZE)
                    })
                    .map_err(|error| error.to_string())?
                } else {
                    queue
                        .lock()
                        .prune_terminal_window(&cursor_key, cutoff, BATCH_SIZE, false)
                        .map_err(|error| error.to_string())?
                };
                Ok(BatchCounts::from_jobs_window(window))
            })
        }
        MaintenancePolicy::PlaybackHistory(policy) => {
            let cutoff = cutoff(now, policy.retention_days)?;
            report.cutoff = Some(cutoff);
            report.dry_run = policy.dry_run;
            let dry_run = policy.dry_run;
            run_batches(&mut report, dry_run, &check_stop, || {
                let store = state.store.as_ref();
                store
                    .prune_playback_logs(cutoff, BATCH_SIZE, dry_run)
                    .map(BatchCounts::from_playback)
                    .map_err(|error| error.to_string())
            })
        }
        MaintenancePolicy::DataRetention(policy) => {
            run_data_retention(state, job, policy, now, &check_stop, &mut report)
        }
    };
    report.elapsed_ms = elapsed_ms(started);

    if report.budget_exhausted {
        tracing::warn!(
            job_id = %job.id, kind = job.kind.as_str(), batches = report.batches,
            deleted = report.deleted, "维护任务预算耗尽，剩余候选留到下一轮"
        );
    }
    if report.note.is_some() {
        tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), note = ?report.note, "维护任务被降级或提前停止");
    }
    let written = write_progress(&queue, job, &report);
    match (outcome, written) {
        (Ok(()), Ok(())) => {
            tracing::info!(
                job_id = %job.id, kind = job.kind.as_str(), batches = report.batches,
                deleted = report.deleted, elapsed_ms = report.elapsed_ms, "维护任务执行完成"
            );
            Ok(())
        }
        (Ok(()), Err(error)) => Err(error),
        (Err(error), _) => {
            tracing::error!(job_id = %job.id, kind = job.kind.as_str(), %error, "维护任务执行失败");
            Err(error)
        }
    }
}

/// 维护任务写入 Queue 的唯一入口。
///
/// Review L6: 旧实现靠 `try_lock` 探测"调用方是否持锁"，既是竞态也表达不了约定。
/// 现在 `jobs::Queue::tick` 在调用 runner 前就释放了 Queue 锁，所以维护任务可以
/// 直接取短锁；把 Queue 包成这个类型，是为了让"维护代码只能通过它取锁"成为类型约定，
/// 而不是每个调用点各自凭记忆加锁。
pub(crate) struct QueueGuard<'a> {
    queue: &'a crate::JobsWriter,
}

impl<'a> QueueGuard<'a> {
    fn new(queue: &'a crate::JobsWriter) -> Self {
        Self { queue }
    }

    /// 取一次 Queue 短锁（批处理边界上释放）。
    fn lock(&self) -> crate::JobsWriterGuard<'a> {
        self.queue.lock()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceStop {
    Active,
    Disabled,
    Cancelled,
    ShuttingDown,
    ReadFailed,
}

impl MaintenanceStop {
    pub fn stop_note(self, unit: &str) -> Option<String> {
        match self {
            Self::Active => None,
            Self::Disabled => Some(format!("任务定义已禁用，未启动下一{unit}")),
            Self::ShuttingDown => Some(format!("服务正在关闭，未启动下一{unit}")),
            Self::Cancelled => Some(format!("任务已取消，未启动下一{unit}")),
            Self::ReadFailed => Some(format!("读取任务状态失败，未启动下一{unit}")),
        }
    }
}

/// 检查维护任务是否应当在批次/库边界停止：
/// 1. 是否正在停机 (shutting down)
/// 2. 对应的 def 是否被禁用 (disabled)
/// 3. 本次 job 状态是否已被取消或变更 (cancelled)
/// 4. 状态读取失败保守停止 (read failed)
pub fn check_maintenance_stop(state: &ApiState, job: &Job) -> MaintenanceStop {
    if state.job_loop_control().is_stopping() {
        return MaintenanceStop::ShuttingDown;
    }
    let observation = crate::jobs_readers::read(&state.jobs_readers, |queue| {
        if let Some(def_id) = job.def_id {
            match queue.def(def_id)? {
                Some(def) if !def.enabled => return Ok(MaintenanceStop::Disabled),
                None => return Ok(MaintenanceStop::Cancelled),
                _ => {}
            }
        }
        Ok(match queue.get(job.id)? {
            Some(current)
                if current.status == JobStatus::Running && current.attempt == job.attempt =>
            {
                MaintenanceStop::Active
            }
            _ => MaintenanceStop::Cancelled,
        })
    });
    match observation {
        Ok(stop) => stop,
        Err(error) => {
            tracing::warn!(job_id = %job.id, %error, "读取维护任务状态失败，保守停止");
            MaintenanceStop::ReadFailed
        }
    }
}

fn cutoff(now: i64, retention_days: u32) -> Result<i64, String> {
    let seconds = i64::from(retention_days)
        .checked_mul(DAY_SECS)
        .ok_or("保留天数换算秒数溢出")?;
    now.checked_sub(seconds)
        .ok_or_else(|| format!("保留窗口下界溢出: now={now} seconds={seconds}"))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn write_progress(
    queue: &QueueGuard<'_>,
    job: &Job,
    report: &MaintenanceReport,
) -> Result<(), String> {
    let json =
        serde_json::to_string(report).map_err(|error| format!("序列化维护报告失败: {error}"))?;
    queue
        .lock()
        .set_job_progress(job.id, &json)
        .map_err(|error| format!("写入维护报告失败: {error}"))
}

// ---------------------------------------------------------------------------
// 数据库维护
// ---------------------------------------------------------------------------

fn run_database(
    state: &ApiState,
    job: &Job,
    policy: &DatabaseMaintenancePolicy,
    now: i64,
    check_stop: &dyn Fn() -> MaintenanceStop,
    report: &mut MaintenanceReport,
) -> Result<(), String> {
    // 只把 owned 路径带出 Store 锁，随后立刻释放。
    let (data_dir, app_path) = {
        let store = state.store.as_ref();
        (
            store.data_dir().to_path_buf(),
            store.app_db_path().to_path_buf(),
        )
    };
    // 预检：Library 工作运行时跳过重型维护，但仍做轻量 optimize（报告里写明原因）。
    let library_busy = state.library_work.has_active_work();
    let vacuum_enabled = policy.vacuum_enabled && !library_busy;
    // L5: 完整 VACUUM 会拿独占锁。准入期间关写门禁：前台写立刻 503 + `Retry-After`，
    // 而不是在 SQLite busy 上耗满 `busy_timeout` 再报 500。默认策略
    // `vacuum_enabled=false`，因此默认安装不会关闸。
    let write_gate = vacuum_enabled.then(|| state.write_gate());
    let _gate_guard = write_gate.as_ref().map(|gate| {
        tracing::info!(
            job_id = %job.id,
            "维护关闸：VACUUM 准入期间前台写入口返回 503"
        );
        gate.close()
    });
    if policy.vacuum_enabled && library_busy {
        report.note =
            Some("Library 工作运行中，本轮跳过重型 VACUUM（仍执行 PRAGMA optimize）".to_string());
    }

    let mut failures = Vec::new();
    // Cancellation/disable is observed at database boundaries. During SQL only
    // the atomic shutdown flag is consulted, so Jobs maintenance never reenters
    // its own writer or performs a filesystem-backed reader checkout under it.
    let cancelled = || state.job_loop_control().is_stopping();
    for target in database_targets(&data_dir, &app_path) {
        let stop = check_stop();
        if let Some(note) = stop.stop_note("库") {
            report.note = Some(note);
            break;
        }
        let history = read_vacuum_history(state, target.name);
        let context = MaintenanceContext {
            now,
            last_vacuum_success: history.last_success,
            vacuum_history_unreadable: history.unreadable,
            cancelled: &cancelled,
        };
        let result = execute_database_target(state, &target, vacuum_enabled, &context);
        let mut entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                tracing::error!(database = target.name.as_str(), %error, "维护执行器无法打开数据库");
                DatabaseReport::failed(target.name.as_str(), error)
            }
        };
        target.finish_report(&mut entry);
        if entry.outcome == DatabaseOutcome::Vacuumed {
            record_vacuum_success(state, target.name, now);
        }
        if entry.outcome == DatabaseOutcome::Skipped {
            tracing::warn!(
                database = entry.name, reason = ?entry.skip_reason,
                "数据库维护跳过该库"
            );
        }
        if entry.outcome == DatabaseOutcome::Failed {
            failures.push(format!(
                "{}: {}",
                entry.name,
                entry.skip_reason.clone().unwrap_or_default()
            ));
        }
        report.databases.push(entry);
    }
    for target in missing_optional_targets(&data_dir) {
        report.databases.push(DatabaseReport::skipped(
            target.name.as_str(),
            "数据库文件不存在（可选库，不创建）",
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} 个数据库维护失败: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

fn execute_database_target(
    state: &ApiState,
    target: &super::database::DatabaseTarget,
    vacuum_enabled: bool,
    context: &MaintenanceContext<'_>,
) -> Result<DatabaseReport, String> {
    let execute = |conn: &rusqlite::Connection| {
        state
            .maintenance_executor
            .execute(conn, target, vacuum_enabled, context)
    };
    let db = match target.name {
        DatabaseName::App => store::Db::App,
        DatabaseName::Subscribe => store::Db::Subscribe,
        DatabaseName::Library => store::Db::Library,
        DatabaseName::Cache => store::Db::Cache,
        DatabaseName::Jobs => {
            return match state.jobs.try_lock_for(std::time::Duration::from_secs(1)) {
                Some(queue) => queue.with_connection(execute),
                None => Ok(DatabaseReport::skipped(
                    target.name.as_str(),
                    "Jobs writer 繁忙，下轮再试",
                )),
            };
        }
    };
    match state.store.with_writer(db, |conn| Ok(execute(conn))) {
        Ok(result) => result,
        Err(store::StoreError::WriteBusy) => Ok(DatabaseReport::skipped(
            target.name.as_str(),
            "数据库 writer 繁忙，下轮再试",
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn vacuum_setting_key(name: DatabaseName) -> String {
    format!("maintenance.vacuum_last_success.{}", name.as_str())
}

struct VacuumHistory {
    last_success: Option<i64>,
    unreadable: bool,
}

fn read_vacuum_history(state: &ApiState, name: DatabaseName) -> VacuumHistory {
    let store = state.store.as_ref();
    match store.get_setting(&vacuum_setting_key(name)) {
        Ok(None) => VacuumHistory {
            last_success: None,
            unreadable: false,
        },
        Ok(Some(raw)) => match raw.parse() {
            Ok(last) => VacuumHistory {
                last_success: Some(last),
                unreadable: false,
            },
            Err(_) => {
                tracing::error!(
                    database = name.as_str(),
                    "VACUUM 冷却时间无法解析，本轮保守跳过"
                );
                VacuumHistory {
                    last_success: None,
                    unreadable: true,
                }
            }
        },
        Err(error) => {
            tracing::error!(
                database = name.as_str(),
                %error,
                "读取 VACUUM 冷却时间失败，本轮保守跳过"
            );
            VacuumHistory {
                last_success: None,
                unreadable: true,
            }
        }
    }
}

/// VACUUM 成功才推进冷却；跳过/失败不写，避免"没做也当作做过"。
fn record_vacuum_success(state: &ApiState, name: DatabaseName, now: i64) {
    let store = state.store.as_ref();
    if let Err(error) = store.put_setting(&vacuum_setting_key(name), &now.to_string()) {
        tracing::error!(database = name.as_str(), %error, "记录 VACUUM 成功时间失败");
    }
}

// ---------------------------------------------------------------------------
// 保留批次
// ---------------------------------------------------------------------------

struct BatchCounts {
    scanned: usize,
    candidates: usize,
    deleted: usize,
    protected: usize,
    protected_truncated: bool,
    has_more: bool,
}

impl BatchCounts {
    /// 原始窗口（含保护行）路径：窗口内每一行都被检视，`window_has_more` 表示窗口外
    /// 仍有未检视行，因此保护行计数只是下界。
    fn from_jobs_window(window: jobs::RetentionWindow) -> Self {
        Self {
            scanned: window.scanned,
            candidates: window.candidates,
            deleted: window.deleted,
            protected: window.protected,
            protected_truncated: window.window_has_more,
            has_more: window.window_has_more,
        }
    }

    fn from_playback(batch: store::PlaybackRetentionBatch) -> Self {
        Self {
            scanned: batch.scanned,
            candidates: batch.candidates,
            deleted: batch.deleted,
            protected: batch.protected,
            protected_truncated: false,
            has_more: batch.has_more,
        }
    }
}

/// 分批执行并累计报告。
///
/// 停止条件：
/// - **dry_run**：只检视一批，然后把 `has_more` 如实映射成 `budget_exhausted`。
///   试运行不删除，再取下一批只会重复检视同一批行（会虚报候选数），所以不能循环；
///   但也**不能**把 `has_more` 吞掉说"已完成"——用户开 dry_run 就是为了先看规模。
/// - **实删**：窗口耗尽（`!has_more`）、stop、或批数/2 秒预算用尽（先到先停）。
///   **不能**用 `deleted == 0` 提前停止：原始窗口可以整窗都是受保护行，此时后面
///   仍有可删历史（B3 §5.1）。游标会推进，所以不会空转死循环。
fn run_batches(
    report: &mut MaintenanceReport,
    dry_run: bool,
    check_stop: &dyn Fn() -> MaintenanceStop,
    mut next: impl FnMut() -> Result<BatchCounts, String>,
) -> Result<(), String> {
    let mut batches = 0usize;
    let mut more_remains = false;
    let mut time_exhausted = false;
    let started = Instant::now();
    while batches < MAX_BATCHES {
        let stop = check_stop();
        if let Some(note) = stop.stop_note("批") {
            report.note = Some(note);
            break;
        }
        // 单条 SQL 步不可中断，所以 2 秒预算只在批间判定（先到先停）。
        if batches > 0 && started.elapsed() >= MAX_BATCH_BUDGET {
            time_exhausted = true;
            more_remains = true;
            report.note = Some(format!(
                "已达到 {} 秒预算，剩余候选留到下一轮",
                MAX_BATCH_BUDGET.as_secs()
            ));
            break;
        }
        let batch = next()?;
        report.scanned += batch.scanned;
        report.candidates += batch.candidates;
        report.deleted += batch.deleted;
        // 原始窗口互不重叠，保护行只被检视一次，因此按 cycle 累加即"已检视保护行数"。
        report.protected += batch.protected;
        // 下界标记只看**最后一批**：最后一批未截断说明整个 cycle 已扫完，
        // 此时累计的保护行数是精确值，不能再沿用中途的"下界"。
        report.protected_is_lower_bound = batch.protected_truncated;
        batches += 1;
        if dry_run {
            more_remains = batch.has_more;
            if batch.has_more {
                report.note = Some("试运行只检视一批；还有更多可删候选未计入".to_string());
            }
            break;
        }
        if !batch.has_more {
            more_remains = false;
            break;
        }
        more_remains = true;
    }
    report.batches = batches;
    report.budget_exhausted = more_remains && (dry_run || batches >= MAX_BATCHES || time_exhausted);
    if time_exhausted {
        tracing::info!(
            batches,
            elapsed_ms = started.elapsed().as_millis() as i64,
            budget_ms = MAX_BATCH_BUDGET.as_millis() as i64,
            "retention 因时间预算停止（与批数上限区分）"
        );
    }
    Ok(())
}
