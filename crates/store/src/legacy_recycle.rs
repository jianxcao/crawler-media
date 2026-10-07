//! One-time recovery for the removed recycle-bin feature.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use rusqlite::{Connection, params};
use serde_json::Value;

use super::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryStage {
    Planned,
    Copied,
    Published,
    Registered,
}

impl RecoveryStage {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Copied => "copied",
            Self::Published => "published",
            Self::Registered => "registered",
        }
    }

    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("copied") => Self::Copied,
            Some("published") => Self::Published,
            Some("registered") => Self::Registered,
            _ => Self::Planned,
        }
    }
}

struct LegacyBinRow {
    id: String,
    original_path: String,
    binned_path: String,
    ledger_json: String,
    recovered_path: Option<String>,
    recovery_stage: RecoveryStage,
}

pub(super) fn restore_recycle_bin(conn: &Connection) -> Result<(), StoreError> {
    if !recycle_bin_exists(conn)? {
        return Ok(());
    }
    ensure_recovery_columns(conn)?;
    for entry in read_entries(conn)? {
        let Some(ledger) = ledger_from_json(&entry.ledger_json) else {
            tracing::warn!(bin_id = %entry.id, "保留损坏的历史回收站记录");
            continue;
        };
        if restore_entry(conn, &entry, ledger) {
            conn.execute("DELETE FROM recycle_bin WHERE id = ?1", params![entry.id])?;
        }
    }
    let remaining: i64 =
        conn.query_row("SELECT COUNT(*) FROM recycle_bin", [], |row| row.get(0))?;
    if remaining == 0 {
        conn.execute_batch("DROP TABLE recycle_bin;")?;
    }
    Ok(())
}

fn recycle_bin_exists(conn: &Connection) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE (type = 'table' OR type = 'view') AND name = 'recycle_bin')",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn ensure_recovery_columns(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('recycle_bin')")?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    if !cols.iter().any(|c| c == "recovered_path") {
        conn.execute_batch("ALTER TABLE recycle_bin ADD COLUMN recovered_path TEXT;")?;
    }
    if !cols.iter().any(|c| c == "recovery_stage") {
        conn.execute_batch("ALTER TABLE recycle_bin ADD COLUMN recovery_stage TEXT;")?;
    }
    Ok(())
}

fn read_entries(conn: &Connection) -> Result<Vec<LegacyBinRow>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT id, original_path, binned_path, ledger_json, recovered_path, recovery_stage FROM recycle_bin",
    )?;
    let rows = statement.query_map([], |row| {
        let stage_str: Option<String> = row.get(5)?;
        Ok(LegacyBinRow {
            id: row.get(0)?,
            original_path: row.get(1)?,
            binned_path: row.get(2)?,
            ledger_json: row.get(3)?,
            recovered_path: row.get(4)?,
            recovery_stage: RecoveryStage::parse(stage_str.as_deref()),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn update_stage(
    conn: &Connection,
    id: &str,
    target: &Path,
    stage: RecoveryStage,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE recycle_bin SET recovered_path = ?1, recovery_stage = ?2 WHERE id = ?3",
        params![target.display().to_string(), stage.as_str(), id],
    )?;
    Ok(())
}

#[cfg(unix)]
fn file_dev_ino(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_dev_ino(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (file_dev_ino(a), file_dev_ino(b)) {
        (Some(ida), Some(idb)) => ida == idb,
        _ => false,
    }
}

fn staging_path_for(target: &Path) -> PathBuf {
    let mut os_str = target.as_os_str().to_os_string();
    os_str.push(".recycle-tmp");
    PathBuf::from(os_str)
}

fn determine_target(entry: &LegacyBinRow, source: &Path, original: &Path) -> Option<PathBuf> {
    if let Some(ref saved) = entry.recovered_path {
        Some(PathBuf::from(saved))
    } else if source.exists() {
        Some(recovery_path(original, &entry.id))
    } else {
        None
    }
}

fn restore_entry(conn: &Connection, entry: &LegacyBinRow, mut ledger: LedgerRow) -> bool {
    let source = Path::new(&entry.binned_path);
    let original = Path::new(&entry.original_path);

    let Some(target) = determine_target(entry, source, original) else {
        tracing::warn!(bin_id = %entry.id, "保留无文件或无法确认归属的历史回收站记录");
        return false;
    };

    let mut current_stage = entry.recovery_stage;
    let staging = staging_path_for(&target);

    // 阶段1：验证或完成到 Published 阶段
    if !reconcile_to_published(conn, entry, source, &target, &staging, &mut current_stage) {
        return false;
    }

    // 阶段2：登记到 Ledger，并更新至 Registered
    ledger.path = target.display().to_string();
    if let Err(error) = insert_ledger(conn, &ledger) {
        tracing::warn!(bin_id = %entry.id, %error, "写入 ledger 失败，保留历史回收站记录");
        return false;
    }
    if let Err(error) = update_stage(conn, &entry.id, &target, RecoveryStage::Registered) {
        tracing::warn!(bin_id = %entry.id, %error, "更新 Registered 状态失败，保留记录");
        return false;
    }

    // 阶段3：清理残留的 source
    if source.exists() {
        if same_file(source, &target) {
            if let Err(e) = std::fs::remove_file(source) {
                tracing::warn!(bin_id = %entry.id, %e, "清理硬链接源文件失败，保留记录待下次重试");
                return false;
            }
        } else if let Err(e) = std::fs::remove_file(source) {
            tracing::warn!(bin_id = %entry.id, %e, "清理源文件失败，保留记录待下次重试");
            return false;
        }
    }

    tracing::info!(
        bin_id = %entry.id,
        source = %entry.binned_path,
        destination = %ledger.path,
        "已恢复历史回收站记录"
    );
    true
}

fn reconcile_to_published(
    conn: &Connection,
    entry: &LegacyBinRow,
    source: &Path,
    target: &Path,
    staging: &Path,
    current_stage: &mut RecoveryStage,
) -> bool {
    // 检查 target 是否已被占
    if target.exists() {
        if source.exists() && same_file(source, target) {
            // hardlink 已就位，直接视作 Published
            *current_stage = RecoveryStage::Published;
            return true;
        }
        if !source.exists() && (*current_stage == RecoveryStage::Published || *current_stage == RecoveryStage::Registered) {
            // 源已清理，目标完整存在且先前已确认发布或注册
            return true;
        }
        if source.exists() && same_file(source, target) {
            // hardlink 相同实体
            return true;
        }
        tracing::error!(bin_id = %entry.id, target = %target.display(), "目标路径缺乏可信身份凭据或被其他文件占据，不覆盖、不认领");
        return false;
    }

    // target 不存在
    if !source.exists() {
        tracing::warn!(bin_id = %entry.id, "源文件与目标文件均不存在");
        return false;
    }

    publish_from_source(conn, entry, source, target, staging, current_stage)
}

fn publish_from_source(
    conn: &Connection,
    entry: &LegacyBinRow,
    source: &Path,
    target: &Path,
    staging: &Path,
    current_stage: &mut RecoveryStage,
) -> bool {
    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!(bin_id = %entry.id, %error, "创建目标目录失败");
            return false;
        }
    }

    // 记录 Planned
    if *current_stage == RecoveryStage::Planned {
        if let Err(error) = update_stage(conn, &entry.id, target, RecoveryStage::Planned) {
            tracing::warn!(bin_id = %entry.id, %error, "持久化 planned 阶段失败");
            return false;
        }
    }

    // 尝试 hardlink
    if let Ok(()) = std::fs::hard_link(source, target) {
        if staging.exists() {
            let _ = std::fs::remove_file(staging);
        }
        if let Err(error) = update_stage(conn, &entry.id, target, RecoveryStage::Published) {
            tracing::warn!(bin_id = %entry.id, %error, "更新 published 阶段失败");
            return false;
        }
        *current_stage = RecoveryStage::Published;
        return true;
    }

    // 跨盘或 hardlink 失败：通过 staging 临时文件复制
    if !copy_to_staging_and_publish(conn, entry, source, target, staging, current_stage) {
        return false;
    }

    true
}

fn copy_to_staging_and_publish(
    conn: &Connection,
    entry: &LegacyBinRow,
    source: &Path,
    target: &Path,
    staging: &Path,
    current_stage: &mut RecoveryStage,
) -> bool {
    if staging.exists() {
        let _ = std::fs::remove_file(staging);
    }
    if let Err(error) = copy_atomic_file(source, staging) {
        let _ = std::fs::remove_file(staging);
        tracing::warn!(bin_id = %entry.id, %error, "复制到临时文件失败");
        return false;
    }
    if let Err(error) = update_stage(conn, &entry.id, target, RecoveryStage::Copied) {
        let _ = std::fs::remove_file(staging);
        tracing::warn!(bin_id = %entry.id, %error, "更新 copied 阶段失败");
        return false;
    }
    *current_stage = RecoveryStage::Copied;

    if let Err(error) = publish_staging(staging, target) {
        tracing::warn!(bin_id = %entry.id, %error, "发布临时文件到目标失败");
        return false;
    }
    if let Err(error) = update_stage(conn, &entry.id, target, RecoveryStage::Published) {
        tracing::warn!(bin_id = %entry.id, %error, "更新 published 阶段失败");
        return false;
    }
    *current_stage = RecoveryStage::Published;
    true
}

fn copy_atomic_file(source: &Path, staging: &Path) -> Result<(), io::Error> {
    let mut input = std::fs::File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn publish_staging(staging: &Path, target: &Path) -> Result<(), io::Error> {
    // 跨盘 rename 或同盘 rename，但必须保证 no-replace。
    // 在支持 hard_link 的同文件系统上，通过 hard_link（天然 no-replace）发布，然后 remove_file staging；
    // 跨设备 fallback 时，如果已存在则返回 AlreadyExists。
    if let Ok(()) = std::fs::hard_link(staging, target) {
        let _ = std::fs::remove_file(staging);
        return Ok(());
    }
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already exists",
        ));
    }
    std::fs::rename(staging, target)?;
    Ok(())
}

fn recovery_path(original: &Path, id: &str) -> PathBuf {
    if !original.exists() {
        return original.to_path_buf();
    }
    let stem = original
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("recovered");
    let extension = original.extension().and_then(|ext| ext.to_str());
    let file_name = match extension {
        Some(extension) => format!("{stem}.recovered-{id}.{extension}"),
        None => format!("{stem}.recovered-{id}"),
    };
    let base = original.with_file_name(file_name);
    unique_path(base)
}

fn unique_path(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }
    let stem = base
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("recovered");
    let extension = base.extension().and_then(|ext| ext.to_str());
    for index in 1.. {
        let file_name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate = base.with_file_name(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded recovery path search")
}

fn insert_ledger(conn: &Connection, row: &LedgerRow) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO ledger (
             id, media_id, path, season, episode, resolution, codec, hdr,
             quality_source, confidence, filter_score
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT DO NOTHING",
        params![
            row.id.to_string(),
            row.media_id.to_string(),
            row.path,
            row.season.map(i64::from),
            row.episode.map(i64::from),
            row.resolution,
            row.codec,
            row.hdr,
            row.quality_source.as_str(),
            row.confidence.as_str(),
            row.filter_score,
        ],
    )?;
    Ok(())
}

fn ledger_from_json(raw: &str) -> Option<LedgerRow> {
    let value: Value = serde_json::from_str(raw).ok()?;
    Some(LedgerRow {
        id: LedgerId::from_str(value["id"].as_str()?).ok()?,
        media_id: MediaId::from_str(value["media_id"].as_str()?).ok()?,
        path: value["path"].as_str()?.to_string(),
        season: value["season"].as_u64().map(|number| number as u32),
        episode: value["episode"].as_u64().map(|number| number as u32),
        resolution: value["resolution"].as_str().map(str::to_string),
        codec: value["codec"].as_str().map(str::to_string),
        hdr: value["hdr"].as_str().map(str::to_string),
        quality_source: match value["quality_source"].as_str() {
            Some("probe") => QualitySource::Probe,
            _ => QualitySource::Release,
        },
        confidence: match value["confidence"].as_str() {
            Some("low") => Confidence::Low,
            _ => Confidence::High,
        },
        filter_score: value["filter_score"].as_i64().map(|number| number as i32),
    })
}
