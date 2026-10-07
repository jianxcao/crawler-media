use std::collections::HashSet;

use domain::{MediaId, MediaKind};

use super::{ProbeManager, ProbeUnit, markers};

pub(super) fn apply_marker_refresh(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
    unit: &ProbeUnit,
) {
    let media_id = match uuid::Uuid::parse_str(&job.media_id) {
        Ok(id) => MediaId::from_uuid(id),
        Err(error) => {
            fail_job(mgr, job, "媒体 ID 无效");
            tracing::error!(%error, job_id = %job.id, media_id = %job.media_id, "持久化声纹任务中的媒体 ID 无效");
            return;
        }
    };
    let seasons = match marker_refresh_seasons(mgr, job) {
        Ok(seasons) if !seasons.is_empty() => seasons,
        Ok(_) => {
            fail_job(mgr, job, "刷新任务中没有有效季信息");
            return;
        }
        Err(error) => {
            fail_job(mgr, job, "读取刷新任务季信息失败");
            tracing::error!(%error, job_id = %job.id, "读取声纹刷新任务季信息失败");
            return;
        }
    };
    let all_rows = match mgr.store.lock().list_ledger() {
        Ok(rows) => rows,
        Err(error) => {
            fail_job(mgr, job, "读取媒体台账失败");
            tracing::error!(%error, job_id = %job.id, "生成声纹刷新结果时读取媒体台账失败");
            return;
        }
    };
    let replacements = match prepare_season_replacements(
        mgr, job, unit, media_id, &seasons, &all_rows,
    ) {
        Ok(replacements) => replacements,
        Err((season, error)) => {
            fail_job(mgr, job, "片头片尾标记准备失败");
            tracing::error!(%error, job_id = %job.id, %media_id, season, "【片头片尾】准备整条目刷新结果失败，旧标记保持可见");
            return;
        }
    };
    if let Err(error) = mgr
        .store
        .lock()
        .complete_marker_refresh(&job.id, &replacements)
    {
        fail_job(mgr, job, "片头片尾标记写入失败");
        tracing::error!(%error, job_id = %job.id, %media_id, seasons = ?seasons, "【片头片尾】多季原子写入失败，旧标记保持可见");
        return;
    }
    log_marker_refresh_success(job, media_id, &seasons);
}

fn log_marker_refresh_success(job: &crate::store::ProbeJob, media_id: MediaId, seasons: &[u32]) {
    tracing::info!(
        job_id = %job.id,
        %media_id,
        seasons = ?seasons,
        completed = job.completed,
        episodes = job.total,
        elapsed_ms = job.elapsed_ms(now_ms()),
        "【片头片尾】持久化整条目刷新完成，全部季标记和章节缓存已原子替换"
    );
}

fn marker_refresh_seasons(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
) -> Result<Vec<u32>, crate::store::StoreError> {
    if let Some(season) = job.season {
        return Ok(vec![season]);
    }
    let store = mgr.store.lock();
    let units = store.probe_job_units(&job.id)?;
    let mut seasons = HashSet::new();
    for unit in units {
        let row = store
            .get_ledger(&unit.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("ledger {}", unit.ledger_id))
            })?;
        seasons.insert(row.season.unwrap_or(1));
    }
    let mut seasons = seasons.into_iter().collect::<Vec<_>>();
    seasons.sort_unstable();
    Ok(seasons)
}

fn prepare_season_replacements(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
    unit: &ProbeUnit,
    media_id: MediaId,
    seasons: &[u32],
    all_rows: &[domain::LedgerRow],
) -> Result<Vec<crate::store::MarkerResultReplacement>, (u32, crate::store::StoreError)> {
    let mut replacements = Vec::with_capacity(seasons.len());
    for season in seasons {
        let row = all_rows
            .iter()
            .find(|row| row.media_id == media_id && row.season.unwrap_or(1) == *season)
            .cloned()
            .ok_or_else(|| {
                (
                    *season,
                    crate::store::StoreError::Missing(format!("season {season} ledger row")),
                )
            })?;
        let mut anchor_unit = unit.clone();
        anchor_unit.row = row;
        anchor_unit.kind = MediaKind::Tv;
        anchor_unit.force_fingerprint = true;
        anchor_unit.overwrite_markers = true;
        let replacement = markers::prepare_marker_replacement(mgr, &anchor_unit)
            .map_err(|error| (*season, error))?;
        tracing::info!(
            job_id = %job.id,
            %media_id,
            season,
            markers = replacement.markers.len(),
            chapter_caches = replacement.chapter_updates.len(),
            "【片头片尾】本季刷新结果已准备，等待整条目原子写入"
        );
        replacements.push(replacement);
    }
    Ok(replacements)
}

fn fail_job(mgr: &ProbeManager, job: &crate::store::ProbeJob, message: &str) {
    if let Err(error) = mgr
        .store
        .lock()
        .finish_probe_job(&job.id, false, Some(message))
    {
        tracing::error!(%error, job_id = %job.id, %message, "持久化声纹任务失败状态失败");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
