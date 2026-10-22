//! 覆盖事实（`domain::FileCoverage`）→ 旧行模型取值语义的机械投影。
//!
//! 本模块只收敛「取哪个单元」与「退化默认」两件事，不含业务分支；调用方仍以旧签名
//! （season/episode 数值或 `Option`、台账 id 文本）取值，命名与旧实现逐字一致。

use domain::{FileCoverage, UnitKey};

/// 新 DDL（`crates/store/src/schema/v1/library.sql` 的 `library_file_units`）允许一个
/// 文件覆盖多个单元（`PRIMARY KEY(file_id, season, episode)`）；本轮机械换型只取
/// **首单元**，按单元扇出留后续任务。取首单元处必须显式告警，不得静默丢失事实。
///
/// live 实测（只读 `data/live/library.db`）：files = 121，units = 121，
/// multi_unit_files = 0，故当前取首单元不丢任何已存在的行。
pub(crate) fn projected_unit(row: &FileCoverage) -> Option<UnitKey> {
    let adopted = row.units.first().copied();
    if row.units.len() > 1 {
        tracing::warn!(
            file_id = %row.file.file_id,
            units = row.units.len(),
            adopted_season = adopted.map(|unit| unit.season),
            adopted_episode = adopted.map(|unit| unit.episode),
            "文件覆盖多个单元，本轮仅采用首单元（多单元扇出留后续任务）"
        );
    }
    adopted
}

/// 旧行模型 `season: Option<u32>` 上退化默认 `unwrap_or(1)` 的集中替代。
/// 全文件单元（`UnitKey::WHOLE`，坐标 -1/无 join 行）复现旧 `None` → 1。
pub(crate) fn projected_season(row: &FileCoverage) -> u32 {
    projected_unit(row).map_or(1, |unit| u32::try_from(unit.season).unwrap_or(1))
}

/// 旧行模型 `episode: Option<u32>` 上退化默认 `unwrap_or(1)` 的集中替代。
pub(crate) fn projected_episode(row: &FileCoverage) -> u32 {
    projected_unit(row).map_or(1, |unit| u32::try_from(unit.episode).unwrap_or(1))
}

/// 旧行模型 `season: Option<u32>` 的**原样**投影（不套退化默认），供
/// `create_probe_job` 保留旧的 `Option<u32>` 形参语义：全文件单元 → `None`。
pub(crate) fn projected_season_opt(row: &FileCoverage) -> Option<u32> {
    projected_unit(row).and_then(|unit| u32::try_from(unit.season).ok())
}

/// 旧行模型 `episode: Option<u32>` 的**原样**投影（不套退化默认）。
pub(crate) fn projected_episode_opt(row: &FileCoverage) -> Option<u32> {
    projected_unit(row).and_then(|unit| u32::try_from(unit.episode).ok())
}

/// 台账 id 文本（紧凑 32 位十六进制或带连字符 UUID）→ 类型化 id。旧模型直接以
/// `&str` 查台账，解析失败的字符串必然查不到行，故 `None` 与「查不到」同一处理。
pub(crate) fn ledger_id(raw: &str) -> Option<domain::LedgerId> {
    raw.parse().ok()
}

/// 持久化探测单元的原始 ledger id 文本 → 当前台账文件。查不到（含 id 无法解析）
/// 即该文件已从台账删除，报旧文案 `file_deleted: ledger {raw}`。
pub(crate) fn coverage_for_ledger_id(
    store: &crate::Store,
    raw: &str,
) -> Result<domain::FileCoverage, crate::store::StoreError> {
    let Some(file_id) = ledger_id(raw) else {
        return Err(crate::store::StoreError::Missing(format!(
            "file_deleted: ledger {}",
            raw
        )));
    };
    store
        .file_coverage_by_id(file_id)?
        .ok_or_else(|| crate::store::StoreError::Missing(format!("file_deleted: ledger {}", raw)))
}

/// 测试专用：派发快照不可读时合成的最小覆盖事实（旧合成行夹具的等价物：
/// 无季集坐标 → 全文件单元；无路径 → 空 PathBuf；其余取旧夹具同值）。
#[cfg(test)]
pub(crate) fn synthetic_coverage(ledger_id: &str) -> FileCoverage {
    super::test_support::synthetic(ledger_id)
}
