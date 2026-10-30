//! Persistent ScanOperation models and methods.

use std::path::PathBuf;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanOperationStatus {
    Queued,
    Running,
    Yielded,
    Completed,
    Partial,
    Cancelled,
    Failed,
}

impl ScanOperationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Yielded => "yielded",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "yielded" => Some(Self::Yielded),
            "completed" => Some(Self::Completed),
            "partial" => Some(Self::Partial),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Yielded)
    }

    /// Every variant, so callers derive a state set instead of copying literals.
    pub const ALL: [Self; 7] = [
        Self::Queued,
        Self::Running,
        Self::Yielded,
        Self::Completed,
        Self::Partial,
        Self::Cancelled,
        Self::Failed,
    ];

    /// A finished operation: no worker can still resume it, so retention may drop the row.
    pub fn is_terminal(&self) -> bool {
        !self.is_active()
    }
}

/// `scan_operations.status IN (...)` for the terminal variants, derived from
/// [`ScanOperationStatus`] so a new status can never be silently treated as terminal.
pub(crate) fn terminal_status_predicate() -> String {
    let values: Vec<String> = ScanOperationStatus::ALL
        .iter()
        .filter(|status| status.is_terminal())
        .map(|status| format!("'{}'", status.as_str()))
        .collect();
    format!("status IN ({})", values.join(","))
}

/// 扫描意图：`full` 表示走完整库根，`targeted` 表示只走 `targets`。
///
/// 之所以要把它和 `targets` 分开持久化：空 `targets` 既可能表示「整库」，
/// 也可能表示「本轮没有新增目标」，只靠数组无法区分（P2-1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ScanScopeKind {
    Full,
    Targeted,
}

impl ScanScopeKind {
    fn from_targets(targets: &[PathBuf]) -> Self {
        if targets.is_empty() {
            Self::Full
        } else {
            Self::Targeted
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Targeted => "targeted",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "full" => Some(Self::Full),
            "targeted" => Some(Self::Targeted),
            _ => None,
        }
    }

    /// 合并规则：Full 支配 Targeted（整库请求永远不会被增量目标缩小）。
    fn merge(self, incoming: Self) -> Self {
        if self == Self::Full || incoming == Self::Full {
            Self::Full
        } else {
            Self::Targeted
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOperation {
    pub id: String,
    pub library_id: String,
    pub scope_signature: String,
    pub status: ScanOperationStatus,
    pub generation: i64,
    pub report: serde_json::Value,
    /// 增量扫描目标（`scope_kind == targeted` 时生效）。
    pub targets: Vec<PathBuf>,
    scope_kind: ScanScopeKind,
    /// 每次并入请求（含重复 target）都自增；worker 据此判断是否需要补扫一轮。
    pub dirty_rev: i64,
    /// worker 已消费到的 `dirty_rev`。
    pub consumed_rev: i64,
}

impl ScanOperation {
    /// 是否整库扫描（`scope_kind == full`）。
    pub fn is_full_scope(&self) -> bool {
        self.scope_kind == ScanScopeKind::Full
    }

    /// 持久化的扫描意图字符串（`full` / `targeted`）。
    pub fn scope_kind_str(&self) -> &'static str {
        self.scope_kind.as_str()
    }
}

fn parse_targets(raw: &str) -> Vec<PathBuf> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn targets_json(targets: &[PathBuf]) -> String {
    serde_json::to_string(targets).unwrap_or_else(|_| "[]".to_string())
}

/// 列顺序固定为：id, library_id, scope_signature, status, generation, report_json,
/// targets_json, scope_kind, dirty_rev, consumed_rev。
fn map_scan_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanOperation> {
    let status_str: String = row.get(3)?;
    let report_json: String = row.get(5)?;
    let targets_raw: String = row.get(6)?;
    let kind_raw: String = row.get(7)?;
    Ok(ScanOperation {
        id: row.get(0)?,
        library_id: row.get(1)?,
        scope_signature: row.get(2)?,
        status: ScanOperationStatus::from_str(&status_str).unwrap_or(ScanOperationStatus::Queued),
        generation: row.get(4)?,
        report: serde_json::from_str(&report_json).unwrap_or_default(),
        targets: parse_targets(&targets_raw),
        // 老库没有 scope_kind：默认 full 是安全方向（整库多扫，绝不缩小）。
        scope_kind: ScanScopeKind::from_str(&kind_raw).unwrap_or(ScanScopeKind::Full),
        dirty_rev: row.get(8)?,
        consumed_rev: row.get(9)?,
    })
}

/// 所有 `ScanOperation` 查询共用的列清单。
const SCAN_OPERATION_COLUMNS: &str = "id, library_id, scope_signature, status, generation, report_json, targets_json, \
     scope_kind, dirty_rev, consumed_rev";

impl Store {
    /// Enqueue a scan operation. If an active scan exists for this library, returns the existing operation.
    ///
    /// `targets` 非空 = 只扫描这些子目录（`targeted`），空 = 整库（`full`）。命中已有 active
    /// operation 时按「Full 支配 Targeted」合并意图，并把新目标并进去（持久化到行上）；
    /// 每次并入（含重复 target）都自增 `dirty_rev`，worker 据此补扫一轮，重复事件不会被吞。
    pub fn enqueue_scan(
        &self,
        library_id: &str,
        signature: &str,
        targets: &[PathBuf],
        now_ms: i64,
    ) -> Result<ScanOperation, StoreError> {
        self.with_library_write(|db| db.enqueue_scan(library_id, signature, targets, now_ms))
    }

    /// Retrieve scan operation by ID.
    pub fn scan_operation(&self, id: &str) -> Result<Option<ScanOperation>, StoreError> {
        self.with_library_read(|db| db.scan_operation(id))
    }

    /// 某个 Library 的扫描 operation，最新在前（排查与测试用）。
    pub fn scan_operations_for_library(
        &self,
        library_id: &str,
    ) -> Result<Vec<ScanOperation>, StoreError> {
        self.with_library_read(|db| db.scan_operations_for_library(library_id))
    }

    /// 推进已消费的 `dirty_rev`：只前进不后退，保证单调。
    pub fn advance_scan_consumed_rev(&self, id: &str, rev: i64) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.advance_scan_consumed_rev(id, rev))
    }

    /// Cancel a scan operation if it is active.
    pub fn cancel_scan(&self, id: &str, _now_ms: i64) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.cancel_scan(id, _now_ms))
    }

    /// Update operation status and report. Does not overwrite terminal statuses (cancelled/completed/failed).
    pub fn update_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.update_scan_operation_status(id, status, report))
    }

    /// 只更新状态、**不动 `report_json`**（P3-NEW-1）。
    ///
    /// 补扫重排的事务刚把跨轮累计报告写进行里；worker 进入 `running` 时若用
    /// [`Self::update_scan_operation_status`] 传空报告，会把这份已提交前缀抹掉，
    /// 崩溃后只剩空报告。终态行（cancelled/completed/failed）不被覆盖。
    pub fn set_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.set_scan_operation_status(id, status))
    }

    /// 终态 CAS：只有 `dirty_rev` 仍等于本轮开始时消费的版本才写终态；否则补扫重排（P3-1）。
    ///
    /// 判定与写入在同一个 IMMEDIATE 事务里完成：另一连接在 worker 判定之后、写终态之前
    /// 并入的事件不会被终态 UPDATE 覆盖。
    ///
    /// 返回 `Some(consumed_rev)` = 期间有新事件，已推进版本并回到 `queued`（调用方不得写终态）；
    /// `None` = 已写终态，或 operation 已不存在/已是终态（未覆盖）。
    pub fn finalize_or_requeue_scan(
        &self,
        id: &str,
        expected_dirty_rev: i64,
        terminal_status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<Option<i64>, StoreError> {
        self.with_library_write(|db| {
            db.finalize_or_requeue_scan(id, expected_dirty_rev, terminal_status, report)
        })
    }

    /// 给已存在的台账行重新挂上扫描 probe 义务（P2-3：文件内容变化后派生事实失效）。
    ///
    /// 义务表是 durable 合同：dispatcher 会在后续轮次按现有 probe 策略重排探测。
    pub fn arm_scan_probe_obligation(
        &self,
        ledger_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.arm_scan_probe_obligation(ledger_id, now_ms, reason))
    }

    /// Atomically claim the oldest queued scan operation, setting its status to running.
    ///
    /// 用 `BEGIN IMMEDIATE`：SELECT 与 UPDATE 必须在同一个写事务里，否则多连接下
    /// 两个 worker 可以同时读到同一行 queued（B3 §6）。
    pub fn claim_next_queued_scan(
        &self,
        excluding: &[String],
    ) -> Result<Option<ScanOperation>, StoreError> {
        self.with_library_write(|db| db.claim_next_queued_scan(excluding))
    }

    /// Recover orphaned running scans on startup: validates if the library still accepts work;
    /// if deleted/deleting, marks cancelled, otherwise re-queues.
    pub fn recover_orphaned_scans(&self) -> Result<usize, StoreError> {
        self.with_library_write(|db| db.recover_orphaned_scans())
    }
}

/// 读取某 Library 当前的 active scan operation（同一 IMMEDIATE 事务内）。
fn active_scan_operation(
    tx: &rusqlite::Connection,
    library_id: &str,
) -> Result<Option<ScanOperation>, StoreError> {
    tx.query_row(
        &format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations
             WHERE library_id = ?1 AND status IN ('queued', 'running', 'yielded')
             LIMIT 1"
        ),
        [library_id],
        map_scan_operation,
    )
    .optional()
    .map_err(Into::into)
}

/// 把新意图并入已有 active operation：Full 支配 Targeted，重复 target 也推进 `dirty_rev`。
fn merge_scan_intent(
    tx: &rusqlite::Connection,
    mut existing: ScanOperation,
    targets: &[PathBuf],
) -> Result<ScanOperation, StoreError> {
    let incoming_kind = ScanScopeKind::from_targets(targets);
    let merged_kind = existing.scope_kind.merge(incoming_kind);
    // Full 支配 Targeted：整库语义下 target 列表没有意义，清空以免被误读为缩小范围。
    let mut merged = if merged_kind == ScanScopeKind::Full {
        Vec::new()
    } else {
        existing.targets.clone()
    };
    let mut added = 0usize;
    if merged_kind == ScanScopeKind::Targeted {
        for target in targets {
            if !merged.contains(target) {
                merged.push(target.clone());
                added += 1;
            }
        }
    }
    // 即使 target 重复、即使意图被 Full 吸收，也必须自增版本：
    // 这是「同一 target 的后续事件不被吞掉」的唯一依据。
    let next_dirty = existing.dirty_rev + 1;
    tx.execute(
        "UPDATE scan_operations
         SET targets_json = ?1, scope_kind = ?2, dirty_rev = ?3
         WHERE id = ?4",
        params![
            targets_json(&merged),
            merged_kind.as_str(),
            next_dirty,
            existing.id
        ],
    )?;
    tracing::info!(
        library_id = %existing.library_id,
        op_id = %existing.id,
        added,
        scope_kind = merged_kind.as_str(),
        dirty_rev = next_dirty,
        "扫描请求已并入进行中的扫描操作"
    );
    existing.targets = merged;
    existing.scope_kind = merged_kind;
    existing.dirty_rev = next_dirty;
    Ok(existing)
}

/// 新建一个 queued scan operation（generation 单调递增）。
fn create_scan_operation(
    tx: &rusqlite::Connection,
    library_id: &str,
    signature: &str,
    targets: &[PathBuf],
    now_ms: i64,
) -> Result<ScanOperation, StoreError> {
    let incoming_kind = ScanScopeKind::from_targets(targets);
    let op_id = uuid::Uuid::new_v4().to_string();
    let max_gen: Option<i64> = tx
        .query_row(
            "SELECT MAX(generation) FROM scan_operations WHERE library_id = ?1",
            [library_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let generation = max_gen.unwrap_or(0) + 1;
    tx.execute(
        "INSERT INTO scan_operations
            (id, library_id, scope_signature, status, generation, created_at_ms, report_json,
             targets_json, scope_kind, dirty_rev, consumed_rev)
         VALUES (?1, ?2, ?3, 'queued', ?4, ?5, '{}', ?6, ?7, 0, 0)",
        params![
            op_id,
            library_id,
            signature,
            generation,
            now_ms,
            targets_json(targets),
            incoming_kind.as_str()
        ],
    )?;
    Ok(ScanOperation {
        id: op_id,
        library_id: library_id.to_string(),
        scope_signature: signature.to_string(),
        status: ScanOperationStatus::Queued,
        generation,
        report: serde_json::json!({}),
        targets: targets.to_vec(),
        scope_kind: incoming_kind,
        // 新建 operation 尚未执行，不需要补扫版本；后续并入请求才会自增。
        dirty_rev: 0,
        consumed_rev: 0,
    })
}

trait RusqliteOptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}
impl<T> RusqliteOptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[path = "scan_operations/connection.rs"]
mod connection;
