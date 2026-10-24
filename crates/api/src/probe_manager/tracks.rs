//! Stream probing and its source publication boundary.
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{ProbeManager, ProbeUnit, projected_episode, projected_season};

/// Stream facts reader; tests inject a deterministic source replacement here.
pub trait TracksProbe: Send + Sync {
    fn probe(
        &self,
        path: &Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError>;
}

pub(super) struct FfprobeTracks;

impl TracksProbe for FfprobeTracks {
    fn probe(
        &self,
        path: &Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        library::probe_tracks_and_duration_controlled(path, control)
    }
}

fn reusable_media_duration(
    mgr: &ProbeManager,
    ledger_id: &str,
    source_version: &str,
) -> Option<i64> {
    let store = mgr.store.lock();
    let cached_version = store.get_media_info_cache_version(&ledger_id);
    let cached_tracks = store.get_file_meta(&ledger_id);
    match (cached_version, cached_tracks) {
        (Ok(Some(version)), Ok(Some(_)))
            if version.source_version == source_version
                && version
                    .format_duration_ms
                    .is_some_and(|duration| duration > 0) =>
        {
            version.format_duration_ms
        }
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!(%error, ledger_id, "【媒体信息】读取版本缓存失败，回退到重新探测");
            None
        }
        _ => None,
    }
}

/// 版本未变化且允许复用缓存时，复用已缓存的流信息与媒体时长（纯抽取自 `probe_tracks_and_store`）。
fn reused_media_duration(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    ledger_id: &str,
    source_version: &str,
    started_at: Instant,
) -> Option<i64> {
    if !unit.reuse_media_info_cache {
        return None;
    }
    let duration_ms = reusable_media_duration(mgr, ledger_id, source_version)?;
    mgr.timings
        .record_media_info(unit.job_id.as_deref(), 0, true);
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id,
        source_version,
        format_duration_ms = ?duration_ms,
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体信息】版本未变化，复用已缓存流信息和媒体时长"
    );
    Some(duration_ms)
}

pub(super) async fn probe_tracks_and_store(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    source_version: &str,
) -> Result<Option<i64>, ()> {
    let ledger_id = unit.row.file.file_id.to_string();
    let path = PathBuf::from(&unit.row.file.path);
    let started_at = Instant::now();
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id,
        path = %path.display(),
        refresh_id = ?unit.marker_refresh_id,
        "【媒体信息】流信息探测开始"
    );
    if let Some(cached) = reused_media_duration(mgr, unit, &ledger_id, source_version, started_at) {
        return Ok(Some(cached));
    }
    let tracks = read_tracks(mgr, unit, started_at).await?;
    let (tracks, duration_ms) = tracks;
    let published = {
        if crate::fingerprint_job::source::observe_source(&unit.row).is_err() {
            return Err(());
        }
        let store = mgr.store.lock();
        if crate::fingerprint_job::source::ensure_source_recorded(&store, &unit.row).is_err() {
            return Err(());
        }
        super::probe::publish_probe_tracks(&store, unit, &source_version, &tracks, duration_ms)
    };
    match published {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                media_id = %unit.row.file.media_id,
                season = projected_season(&unit.row),
                episode = projected_episode(&unit.row),
                ledger_id,
                source_version,
                "【媒体信息】探测结果属于已被替换的文件版本，丢弃不发布"
            );
            return Err(());
        }
        Err(error) => {
            tracing::error!(
                %error,
                media_id = %unit.row.file.media_id,
                season = projected_season(&unit.row),
                episode = projected_episode(&unit.row),
                ledger_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【媒体信息】流信息缓存写入失败"
            );
            return Err(());
        }
    }
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id,
        path = %path.display(),
        video = tracks.video.is_some(),
        audio_tracks = tracks.audio.len(),
        subtitle_tracks = tracks.subtitles.len(),
        source_version,
        format_duration_ms = ?duration_ms,
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体信息】流信息探测与缓存完成"
    );
    mgr.timings.record_media_info(
        unit.job_id.as_deref(),
        started_at.elapsed().as_millis() as u64,
        false,
    );
    Ok(duration_ms)
}

async fn read_tracks(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    started_at: Instant,
) -> Result<(library::Tracks, Option<i64>), ()> {
    let ledger_id = unit.row.file.file_id.to_string();
    let path = &unit.row.file.path;
    let probe_unit = unit.clone();
    let control = probe_control(mgr, unit);
    let engine = mgr.tracks_probe.clone();
    let tracks_result = tokio::task::spawn_blocking(move || {
        read_tracks_with_nfo(&probe_unit, started_at, &control, engine.as_ref())
    })
    .await;
    let tracks = match tracks_result {
        Ok(Some(tracks_and_duration)) => tracks_and_duration,
        Ok(None) => {
            mgr.timings.record_media_info(
                unit.job_id.as_deref(),
                started_at.elapsed().as_millis() as u64,
                false,
            );
            return Err(());
        }
        Err(error) => {
            tracing::error!(
                %error,
                media_id = %unit.row.file.media_id,
                season = projected_season(&unit.row),
                episode = projected_episode(&unit.row),
                ledger_id,
                path = %path.display(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【媒体信息】流信息探测任务异常退出"
            );
            mgr.timings.record_media_info(
                unit.job_id.as_deref(),
                started_at.elapsed().as_millis() as u64,
                false,
            );
            return Err(());
        }
    };
    Ok(tracks)
}

pub(super) fn probe_control(mgr: &ProbeManager, unit: &ProbeUnit) -> library::WorkControl {
    let owner = unit.row.file.library_id.to_string();
    let registry = mgr.library_work.lock().clone();
    registry
        .and_then(|registry| registry.active_control(&owner))
        .unwrap_or_default()
}

fn read_tracks_with_nfo(
    unit: &ProbeUnit,
    probe_started_at: Instant,
    control: &library::WorkControl,
    engine: &dyn TracksProbe,
) -> Option<(library::Tracks, Option<i64>)> {
    if crate::fingerprint_job::source::observe_source(&unit.row).is_err() {
        return None;
    }
    let tracks_path = PathBuf::from(&unit.row.file.path);
    let nfo_path = tracks_path.with_extension("nfo");
    let media_id = unit.row.file.media_id;
    let season = projected_season(&unit.row);
    let episode = projected_episode(&unit.row);
    let probe_ledger_id = unit.row.file.file_id.to_string();
    match engine.probe(&tracks_path, control) {
        Ok((tracks, duration_ms)) => {
            if crate::fingerprint_job::source::observe_source(&unit.row).is_err() {
                return None;
            }
            let has_streams =
                tracks.video.is_some() || !tracks.audio.is_empty() || !tracks.subtitles.is_empty();
            if control.is_cancelled() {
                return None;
            }
            if has_streams {
                if let Err(error) = control.publish(|| {
                    library::write_streamdetails_into_nfo(
                        &nfo_path,
                        tracks.video.as_ref(),
                        &tracks.audio,
                        &tracks.subtitles,
                    )
                    .map_err(|error| error.to_string())
                }) {
                    tracing::warn!(
                        media_id = %media_id,
                        season,
                        episode,
                        ledger_id = %probe_ledger_id,
                        path = %tracks_path.display(),
                        nfo_path = %nfo_path.display(),
                        error = %error,
                        elapsed_ms = probe_started_at.elapsed().as_millis() as u64,
                        "【媒体信息】写入 NFO 流信息失败"
                    );
                } else {
                    tracing::debug!(
                        media_id = %media_id,
                        season,
                        episode,
                        ledger_id = %probe_ledger_id,
                        nfo_path = %nfo_path.display(),
                        elapsed_ms = probe_started_at.elapsed().as_millis() as u64,
                        "【媒体信息】NFO 流信息写入完成或无需更新"
                    );
                }
            } else {
                tracing::warn!(
                    media_id = %media_id,
                    season,
                    episode,
                    ledger_id = %probe_ledger_id,
                    path = %tracks_path.display(),
                    elapsed_ms = probe_started_at.elapsed().as_millis() as u64,
                    "【媒体信息】探测成功但没有发现视频、音轨或字幕流"
                );
            }
            Some((tracks, duration_ms))
        }
        Err(error) => {
            tracing::warn!(
                media_id = %media_id,
                season,
                episode,
                ledger_id = %probe_ledger_id,
                path = %tracks_path.display(),
                error = %error,
                elapsed_ms = probe_started_at.elapsed().as_millis() as u64,
                "【媒体探测】探测失败（文件不可达 / 远程超时），稍后重试"
            );
            None
        }
    }
}
