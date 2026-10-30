use std::path::Path;

use domain::{JobDefId, JobId};
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::kinds::{JobKind, Schedule};
use crate::retention::RetentionBatch;
use crate::store;
use crate::types::{Job, JobDef, JobError, JobStatus};

pub struct NewDef {
    pub kind: JobKind,
    pub name: String,
    pub enabled: bool,
    pub schedule: Option<Schedule>,
    pub payload: String,
    pub timeout_secs: Option<u32>,
    pub concurrency_key: Option<String>,
}

pub trait Runner {
    fn run(&self, job: &Job) -> Result<(), String>;
}

pub struct Queue {
    conn: Connection,
}

impl Queue {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JobError> {
        let path = path.as_ref();
        crate::schema::preflight(path).inspect_err(|error| {
            tracing::error!(path = %path.display(), %error, "Jobs layout preflight rejected");
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // Create path only：`auto_vacuum` 写在库头里，SQLite 只在“没有表且不是 WAL”
        // 时改写（否则静默 no-op），所以必须早于下面切 WAL 与任何 DDL。维护任务据此
        // 才能用 `PRAGMA incremental_vacuum` 归还空闲页（`maintenance/database.rs`
        // 会先读这个 pragma，不支持就如实报“非 incremental”）。
        apply_auto_vacuum_on_create(&conn)?;
        apply_connection_policy(&conn)?;
        crate::schema::initialize_or_validate(&conn).inspect_err(|error| {
            tracing::error!(path = %path.display(), %error, "Jobs initialization rejected");
        })?;
        Ok(Self { conn })
    }

    /// Borrow this Queue's actual writer for database maintenance. The caller
    /// retains its writer guard; no parallel connection is opened.
    pub fn with_connection<T>(&self, work: impl FnOnce(&Connection) -> T) -> T {
        work(&self.conn)
    }

    pub fn enqueue(&self, kind: JobKind, payload: &str, run_after: i64) -> Result<Job, JobError> {
        self.enqueue_with_key(kind, payload, run_after, None)
    }

    pub fn enqueue_with_key(
        &self,
        kind: JobKind,
        payload: &str,
        run_after: i64,
        concurrency_key: Option<&str>,
    ) -> Result<Job, JobError> {
        tracing::debug!(kind = kind.as_str(), run_after, concurrency_key, "任务入队");
        store::insert_job(
            &self.conn,
            None,
            kind,
            payload,
            run_after,
            concurrency_key,
            None,
        )
    }

    pub fn upsert_def(&self, def: NewDef) -> Result<JobDef, JobError> {
        store::upsert_def(&self.conn, def)
    }

    pub fn ensure_def(&self, def: NewDef) -> Result<JobDef, JobError> {
        store::ensure_def(&self.conn, def)
    }

    pub fn list_defs(&self) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::list_defs(&self.conn)
    }

    pub fn def(&self, id: JobDefId) -> Result<Option<JobDef>, JobError> {
        crate::schedule::get_def(&self.conn, id)
    }

    pub fn def_name(&self, id: JobDefId) -> Result<Option<String>, JobError> {
        Ok(self.def(id)?.map(|def| def.name))
    }

    pub fn claim(&self, now: i64, limit: usize) -> Result<Vec<Job>, JobError> {
        let jobs = store::claim(&self.conn, now, limit)?;
        if !jobs.is_empty() {
            tracing::info!(count = jobs.len(), "已领取待执行任务");
        }
        Ok(jobs)
    }

    pub fn rewrite_media_refs(&self, from: &str, to: &str) -> Result<usize, JobError> {
        crate::media_refs::rewrite_media_refs(&self.conn, from, to)
    }

    pub fn succeed(&self, id: JobId, now: i64) -> Result<Job, JobError> {
        let job = store::finish(&self.conn, id, now, JobStatus::Succeeded, None)?;
        tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "任务执行成功");
        Ok(job)
    }

    /// Commit only if this is still the execution that claimed the Job.
    pub fn succeed_claimed(&self, claimed: &Job, now: i64) -> Result<Option<Job>, JobError> {
        store::finish_claimed(&self.conn, claimed, now, JobStatus::Succeeded)
    }

    pub fn fail_claimed(
        &self,
        claimed: &Job,
        now: i64,
        error: &str,
    ) -> Result<Option<Job>, JobError> {
        store::fail_claimed(&self.conn, claimed, now, error)
    }

    /// 放回队列为 queued，保持原 attempt 与 run_after，不产生失败记录
    pub fn requeue_without_attempt(&self, job: &Job) -> Result<(), JobError> {
        let _ = self.conn.execute(
            "UPDATE jobs SET status = 'queued', started_at = NULL WHERE id = ?1 AND status = 'running'",
            rusqlite::params![job.id.to_string()],
        )?;
        Ok(())
    }

    pub fn fail(&self, id: JobId, now: i64, error: &str) -> Result<Job, JobError> {
        store::fail(&self.conn, id, now, error)
    }

    pub fn recover(&self, now: i64) -> Result<Vec<Job>, JobError> {
        let recovered = store::recover(&self.conn, now)?;
        if !recovered.is_empty() {
            tracing::warn!(count = recovered.len(), "恢复超时任务");
        }
        Ok(recovered)
    }

    /// Recover timed-out jobs except attempts known to still be executing in
    /// this process. This preserves their running state and concurrency keys;
    /// ordinary recovery remains available after a process restart.
    pub fn recover_except(&self, now: i64, active: &[Job]) -> Result<Vec<Job>, JobError> {
        let recovered = store::recover_except(&self.conn, now, active)?;
        if !recovered.is_empty() {
            tracing::warn!(count = recovered.len(), "恢复超时任务");
        }
        Ok(recovered)
    }

    pub fn ensure_scheduled(&self, now: i64) -> Result<Vec<Job>, JobError> {
        let created = crate::schedule::ensure_scheduled(&self.conn, now)?;
        if !created.is_empty() {
            tracing::debug!(count = created.len(), "按计划生成新任务");
        }
        Ok(created)
    }

    /// 只调度/提前一个 def（按 id），不碰其他 def。见 schedule::ensure_scheduled_def。
    pub fn ensure_scheduled_for(&self, now: i64, def_id: JobDefId) -> Result<Vec<Job>, JobError> {
        let Some(def) = crate::schedule::get_def(&self.conn, def_id)? else {
            return Ok(Vec::new());
        };
        crate::schedule::ensure_scheduled_def(&self.conn, now, &def)
    }

    /// 只调度/提前 payload 匹配的 def（订阅搜索 / 目录刷新种子用）。
    pub fn ensure_scheduled_for_payload(
        &self,
        now: i64,
        payload: &str,
    ) -> Result<Vec<Job>, JobError> {
        let mut created = Vec::new();
        for def in crate::schedule::defs_for_payload(&self.conn, payload)? {
            created.extend(crate::schedule::ensure_scheduled_def(
                &self.conn, now, &def,
            )?);
        }
        Ok(created)
    }

    pub fn get(&self, id: JobId) -> Result<Option<Job>, JobError> {
        store::get_job(&self.conn, id)
    }

    /// Delete defs whose payload exactly matches, plus their child jobs.
    pub fn delete_defs_for_payload(&self, payload: &str) -> Result<usize, JobError> {
        store::delete_defs_for_payload(&self.conn, payload)
    }

    pub fn set_def_enabled_by_id(
        &self,
        def_id: JobDefId,
        enabled: bool,
    ) -> Result<usize, JobError> {
        store::set_def_enabled_by_id(&self.conn, def_id, enabled)
    }

    /// 事务内原子比较并更新 def 的 payload 和可选 schedule，并刷新排队中的 queued 任务。
    pub fn update_def_config_cas(
        &self,
        def_id: JobDefId,
        expected_payload: &str,
        new_payload: &str,
        new_schedule: Option<Option<&Schedule>>,
    ) -> Result<crate::PayloadUpdate, JobError> {
        store::update_def_config_cas(
            &self.conn,
            def_id,
            expected_payload,
            new_payload,
            new_schedule,
        )
    }

    /// 事务内比较并更新 def 的 payload（维护配置 CAS，DB8 4.2）。
    /// 并发令牌是**保存前的原 payload**，不是 payload 内的 schema `version`。
    pub fn set_def_payload_cas(
        &self,
        def_id: JobDefId,
        expected_payload: &str,
        payload: &str,
    ) -> Result<crate::PayloadUpdate, JobError> {
        store::set_def_payload_by_id(&self.conn, def_id, expected_payload, payload)
    }

    pub fn prune_terminal_window(
        &self,
        cursor_key: &str,
        cutoff: i64,
        limit: usize,
        dry_run: bool,
    ) -> Result<crate::retention_cursor::RetentionWindow, JobError> {
        crate::retention_cursor::prune_terminal_window(
            &self.conn, cursor_key, cutoff, limit, dry_run,
        )
    }

    pub fn change_state(&self) -> Result<crate::change_state::JobChangeState, JobError> {
        crate::change_state::get_job_change_state(&self.conn)
    }
    /// Enable or disable defs whose payload matches. Disabled defs are not scheduled,
    /// and queued children are cancelled by the store layer.
    pub fn set_def_enabled(&self, payload: &str, enabled: bool) -> Result<usize, JobError> {
        store::set_def_enabled(&self.conn, payload, enabled)
    }

    pub fn set_def_schedule(&self, payload: &str, schedule: Schedule) -> Result<usize, JobError> {
        store::set_def_schedule(&self.conn, payload, &schedule)
    }

    pub fn set_def_schedule_by_id(
        &self,
        def_id: JobDefId,
        schedule: Option<Schedule>,
    ) -> Result<usize, JobError> {
        store::set_def_schedule_by_id(&self.conn, def_id, schedule.as_ref())
    }

    pub fn defs_for_payload(&self, payload: &str) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::defs_for_payload(&self.conn, payload)
    }

    /// Recent child jobs (any status) whose payload matches, newest first.
    pub fn jobs_for_payload(&self, payload: &str, limit: usize) -> Vec<Job> {
        store::jobs_for_payload(&self.conn, payload, limit).unwrap_or_default()
    }

    /// Cancel queued children of a def; running external work is allowed to finish.
    pub fn cancel_def_children(&self, def_id: JobDefId) -> Result<usize, JobError> {
        store::cancel_def_children(&self.conn, def_id)
    }

    /// Count jobs finished after `since` (used by the /jobs/stream watcher).
    pub fn count_finished_since(&self, since: i64) -> Result<i64, JobError> {
        store::count_finished_since(&self.conn, since)
    }

    pub fn get_live_child(&self, def_id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_live_child(&self.conn, def_id)
    }

    /// 同一 def 可以同时有一条 running 和一条 queued。`get_live_child` 优先返回 queued，
    /// 所以“是否正在执行”必须单独问。
    pub fn has_running_child(&self, def_id: JobDefId) -> Result<bool, JobError> {
        crate::schedule::has_running_child(&self.conn, def_id)
    }

    pub fn get_last_child(&self, def_id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_last_child(&self.conn, def_id)
    }

    /// 终止态 Jobs 历史的按龄分批清理（DB8 Task 2）。每批一个事务；`dry_run` 零写入。
    pub fn prune_terminal_jobs(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<RetentionBatch, JobError> {
        crate::retention::prune_terminal_jobs(&self.conn, cutoff, batch_size, dry_run)
    }

    /// 批量取每个 def 的 live/last child（≤500 分块），规则与逐个查询一致。
    pub fn children_for_defs(&self, ids: &[JobDefId]) -> Result<Vec<crate::JobChildren>, JobError> {
        crate::read::children_for_defs(&self.conn, ids)
    }

    /// 只写 `progress` 列（单次执行的汇总报告 JSON）。终态持久化不覆盖该列，
    /// 因此执行结束后仍可读；不影响调度/重试语义。
    pub fn set_job_progress(&self, id: JobId, progress: &str) -> Result<(), JobError> {
        let changed = self.conn.execute(
            "UPDATE jobs SET progress = ?2 WHERE id = ?1",
            rusqlite::params![id.to_string(), progress],
        )?;
        if changed == 0 {
            return Err(JobError::UnknownJob(id));
        }
        Ok(())
    }

    /// 一轮调度：持锁恢复/排期/认领 → **释放锁**执行 runner → 再持锁落库。
    ///
    /// Review L6: 旧实现在持有 Queue 锁时调用 runner，runner 只要再碰一次队列
    /// （维护 runner 取短锁做批处理）就会在同一把不可重入的锁上自锁。现在锁只在
    /// 「取任务」和「落库」两小段里持有，执行期间队列对任何线程都是空闲的。
    ///
    /// 执行阶段出错不会中断整轮：每个任务都按自己的结果落库，`error` 只承载
    /// 持锁阶段（恢复/排期/认领/落库）的失败。
    pub fn tick(queue: &Mutex<Queue>, runner: &dyn Runner, now: i64) -> TickReport {
        let claimed = {
            let guard = queue.lock();
            match guard.claim_round(now) {
                Ok(claimed) => claimed,
                Err(error) => {
                    return TickReport {
                        finished: Vec::new(),
                        error: Some(error),
                    };
                }
            }
        };
        let mut finished = Vec::new();
        for job in claimed {
            let outcome = runner.run(&job);
            let persisted = {
                let guard = queue.lock();
                match outcome {
                    Ok(()) => guard.succeed_claimed(&job, now),
                    Err(error) => guard.fail_claimed(&job, now, &error),
                }
            };
            match persisted {
                Ok(Some(job)) => finished.push(job),
                Ok(None) => {}
                Err(error) => {
                    return TickReport {
                        finished,
                        error: Some(error),
                    };
                }
            }
        }
        TickReport {
            finished,
            error: None,
        }
    }

    /// 一轮的持锁部分：恢复超时执行、补排期、认领至多两条（`claim` 的上限）。
    fn claim_round(&self, now: i64) -> Result<Vec<Job>, JobError> {
        self.recover(now)?;
        self.ensure_scheduled(now)?;
        self.claim(now, 2)
    }
}

/// 一次 [`Queue::tick`] 的结果。
///
/// 任务执行发生在 Queue 锁之外，因此这里只回传落库后的终态任务与持锁阶段的错误。
pub struct TickReport {
    /// 本轮已落库的终态任务（成功或失败）。
    pub finished: Vec<Job>,
    /// 持锁阶段（恢复/排期/认领/落库）的错误；`None` 表示整轮完整走完。
    pub error: Option<JobError>,
}

/// 页缓存 16 MiB / 连接（负值 = KiB）。与 `store::open::CACHE_SIZE_KIB` 同值：
/// jobs.db 与四个 Store 库同目录、同机器、同用户，调优不该分家。
const CACHE_SIZE_KIB: i64 = -16_384;
/// WAL 上限 64 MiB（checkpoint 后截断）。
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 67_108_864;
/// mmap 上限 256 MiB / 连接（地址空间上限，不是常驻内存；见
/// `store::open::apply_connection_policy` 的说明）。
const MMAP_SIZE_BYTES: i64 = 268_435_456;
/// `temp_store = MEMORY`：临时表/排序走内存。
const TEMP_STORE_MEMORY: i64 = 2;
/// 打开时带 hint 的 `PRAGMA optimize`（0x10000 = 有益时才 ANALYZE）。
const OPTIMIZE_ON_OPEN: i64 = 0x10002;

/// jobs 队列连接的**连接级**策略，与 Store 的四个库保持一致。
///
/// `busy_timeout`/`foreign_keys` 是连接级设置（bundled 构建默认 5000ms/ON），
/// 显式设置是为了不依赖构建默认值；`journal_mode=WAL` 与 `auto_vacuum` 是持久属性。
/// 数值必须与 `crates/store/src/open.rs` 的同名常量保持一致（两个 crate 没有共享
/// 依赖：jobs 不依赖 store，store 的 dev-dependencies 才依赖 jobs）。
fn apply_connection_policy(conn: &Connection) -> Result<(), JobError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // FULL is durability, not a tuning choice: WAL + NORMAL drops the last commit
    // on power loss, so the queue must not depend on the bundled default.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;
    conn.pragma_update(None, "temp_store", TEMP_STORE_MEMORY)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    conn.pragma_update(None, "mmap_size", MMAP_SIZE_BYTES)?;
    conn.execute_batch(&format!("PRAGMA optimize={OPTIMIZE_ON_OPEN};"))?;
    Ok(())
}

/// Create path 的 `auto_vacuum=INCREMENTAL`：只在**未初始化**的文件上执行。
///
/// 判定与 `crate::schema::is_empty` 同一口径（两个头都为 0 且没有业务对象），
/// 但必须在 `initialize_or_validate` 之前、在切 WAL 之前跑。
fn apply_auto_vacuum_on_create(conn: &Connection) -> Result<(), JobError> {
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id != 0 || user_version != 0 {
        return Ok(());
    }
    let empty: bool = conn.query_row(
        "SELECT count(*)=0 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if !empty {
        return Ok(());
    }
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    Ok(())
}

/// 关闭连接前跑一次 `PRAGMA optimize`（SQLite 推荐的「关闭时」用法）。
impl Drop for Queue {
    fn drop(&mut self) {
        if let Err(error) = self.conn.execute_batch("PRAGMA optimize;") {
            tracing::warn!(%error, "关闭 jobs 连接前的 PRAGMA optimize 失败");
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 必须在**我们自己打开的连接**上断言。
    #[test]
    fn queue_connection_policy_sets_timeout_and_foreign_keys() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_connection_policy(&conn).unwrap();
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        assert_eq!(foreign_keys, 1);
    }

    /// Durability must not ride the bundled default: a connection lowered to OFF
    /// is pulled back to FULL by the queue policy.
    #[test]
    fn queue_connection_policy_forces_synchronous_full() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "synchronous", "OFF").unwrap();
        apply_connection_policy(&conn).unwrap();
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 2, "SQLite FULL");
    }

    /// jobs.db 与 Store 四个库同一套连接调优（同目录、同机器、同用户）。
    ///
    /// 用文件库：`journal_size_limit` 只对真实 WAL 文件有意义，`:memory:` 上读不到行。
    #[test]
    fn queue_connection_policy_matches_store_tuning() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("policy.db")).unwrap();
        apply_connection_policy(&conn).unwrap();
        for (name, expected) in [
            ("cache_size", super::CACHE_SIZE_KIB),
            ("temp_store", super::TEMP_STORE_MEMORY),
            ("journal_size_limit", super::JOURNAL_SIZE_LIMIT_BYTES),
            ("mmap_size", super::MMAP_SIZE_BYTES),
        ] {
            let value: i64 = conn
                .pragma_query_value(None, name, |row| row.get(0))
                .unwrap();
            assert_eq!(value, expected, "{name} 必须与 Store 的连接策略一致");
        }
    }

    /// 建库路径把 auto_vacuum 写成 INCREMENTAL(2)，且重开不得翻转。
    #[test]
    fn create_path_sets_incremental_auto_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        drop(super::Queue::open(&path).unwrap());
        for _ in 0..2 {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let mode: i64 = conn
                .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
                .unwrap();
            assert_eq!(mode, 2, "jobs.db 建库时必须设 incremental auto_vacuum");
            drop(conn);
            // 重开一次，确认 create-path 判定不会把已有库当成新库改回去。
            drop(super::Queue::open(&path).unwrap());
        }
    }
}
