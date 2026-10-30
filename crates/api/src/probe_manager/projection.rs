//! File identity helpers. Physical probing never selects a logical episode.

use domain::{FileCoverage, UnitKey};

pub(crate) fn require_single_unit(row: &FileCoverage) -> Result<UnitKey, crate::store::StoreError> {
    match row.units.as_slice() {
        [unit] => Ok(*unit),
        _ => {
            tracing::error!(file_id = %row.file.file_id, units = row.units.len(),
                error_kind = "unit_time_mapping_unsupported", "automatic marker detection requires unit time mapping");
            Err(crate::store::StoreError::Invalid("unit_time_mapping_unsupported: automatic marker detection requires one mapped unit per file".into()))
        }
    }
}

pub(crate) fn require_episode_coordinates(
    row: &FileCoverage,
) -> Result<(u32, u32), crate::store::StoreError> {
    Ok(require_single_unit(row)?
        .episode_coordinates()
        .unwrap_or((1, 1)))
}

pub(crate) fn single_season(row: &FileCoverage) -> Option<u32> {
    match row.units.as_slice() {
        [unit] => Some(unit.episode_coordinates().map_or(1, |(season, _)| season)),
        _ => None,
    }
}

pub(crate) fn single_episode(row: &FileCoverage) -> Option<u32> {
    match row.units.as_slice() {
        [unit] => Some(unit.episode_coordinates().map_or(1, |(_, episode)| episode)),
        _ => None,
    }
}

pub(crate) fn single_season_opt(row: &FileCoverage) -> Option<u32> {
    match row.units.as_slice() {
        [unit] => unit.episode_coordinates().map(|(season, _)| season),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn single_episode_opt(row: &FileCoverage) -> Option<u32> {
    match row.units.as_slice() {
        [unit] => unit.episode_coordinates().map(|(_, episode)| episode),
        _ => None,
    }
}

pub(crate) fn covers_season(row: &FileCoverage, season: u32) -> bool {
    row.units.iter().any(|unit| {
        unit.episode_coordinates()
            .is_some_and(|coordinates| coordinates.0 == season)
    })
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
    store.library_read_snapshot(|db| coverage_for_ledger_id_on(db, raw))?
}

pub(crate) fn coverage_for_ledger_id_on(
    store: &store::LibraryConnection<'_>,
    raw: &str,
) -> Result<domain::FileCoverage, store::StoreError> {
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
