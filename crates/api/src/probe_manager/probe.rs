use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use domain::MediaKind;
use marker::FingerprintEngine;

use super::{ProbeManager, ProbeUnit};
use super::{projected_episode, projected_season};
use crate::scrape_store::ScrapeStoreExt;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

pub(super) struct FingerprintWork {
    pub(super) unit: ProbeUnit,
    pub(super) media_duration_ms: Option<i64>,
    pub(super) source_version: String,
    pub(super) duration_secs: u32,
    pub(super) start_job: bool,
}

pub(super) enum MetadataOutcome {
    Complete,
    Fingerprint(FingerprintWork),
    Failed,
}

/// Finish mandatory media information before scheduling optional fingerprint work.
pub(super) async fn probe_metadata(mgr: &ProbeManager, unit: &ProbeUnit) -> MetadataOutcome {
    let started_at = Instant::now();
    let job_id = mgr.active_probe_job_id(&unit.row.file.file_id.to_string());
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        "【媒体信息】高优先级探测开始"
    );
    let outcome = probe_metadata_inner(mgr, unit).await;
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        result = match &outcome {
            MetadataOutcome::Complete => "complete",
            MetadataOutcome::Fingerprint(_) => "fingerprint_ready",
            MetadataOutcome::Failed => "failed",
        },
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体信息】高优先级探测结束"
    );
    outcome
}

async fn probe_metadata_inner(mgr: &ProbeManager, unit: &ProbeUnit) -> MetadataOutcome {
    let path = PathBuf::from(&unit.row.file.path);
    let source_version = crate::fingerprint_job::current_source_version(&path);
    let media_duration_ms = match probe_tracks_and_store(mgr, unit, &source_version).await {
        Ok(duration_ms) => duration_ms,
        Err(()) => return MetadataOutcome::Failed,
    };
    if unit.kind != MediaKind::Tv {
        return MetadataOutcome::Complete;
    }
    let Some(duration_secs) = fingerprint_duration(mgr, &path, unit.kind, unit.force_fingerprint)
    else {
        tracing::info!(
            media_id = %unit.row.file.media_id,
            path = %path.display(),
            forced = unit.force_fingerprint,
            "【声纹】媒体库未启用声纹比对或文件不属于可识别媒体库，跳过声纹生成"
        );
        return if unit.force_fingerprint {
            MetadataOutcome::Failed
        } else {
            MetadataOutcome::Complete
        };
    };
    MetadataOutcome::Fingerprint(FingerprintWork {
        unit: ProbeUnit {
            force_fingerprint: true,
            ..unit.clone()
        },
        media_duration_ms,
        source_version,
        duration_secs,
        start_job: false,
    })
}

/// Run optional voiceprint extraction and season comparison on its own worker.
pub(super) async fn probe_fingerprint(mgr: &ProbeManager, work: &FingerprintWork) -> bool {
    matches!(
        probe_fingerprint_detailed(mgr, work).await,
        super::stages::FingerprintRunOutcome::Complete
    )
}

pub(super) async fn probe_fingerprint_detailed(
    mgr: &ProbeManager,
    work: &FingerprintWork,
) -> super::stages::FingerprintRunOutcome {
    let unit = &work.unit;
    let started_at = Instant::now();
    let job_id = unit.job_id.clone();
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        marker_refresh = unit.marker_refresh_id.is_some(),
        refresh_id = ?unit.marker_refresh_id,
        "【声纹】媒体信息已写库，进入低优先级声纹队列"
    );
    // Check if adaptive sampling mode is enabled for TV episodes
    let sampling_mode = {
        let store = mgr.store.lock();
        crate::scrape_store::ScrapeStoreExt::get_scrape_config(&*store)
            .ok()
            .map(|c| c.effective.fingerprint_sampling_mode)
            .unwrap_or_else(|| "full_window".to_string())
    };

    if sampling_mode == "adaptive" && unit.kind == MediaKind::Tv {
        if let Some(result) = probe_adaptive_season(mgr, unit, started_at).await {
            return if result {
                super::stages::FingerprintRunOutcome::Complete
            } else {
                super::stages::FingerprintRunOutcome::Failed {
                    error_kind: "adaptive_failure".into(),
                    error: "Adaptive season failed".into(),
                }
            };
        }
    }

    let path = PathBuf::from(&unit.row.file.path);
    let mut outcome = probe_fingerprint_and_store_detailed(
        mgr,
        unit,
        path,
        work.duration_secs,
        work.media_duration_ms,
        work.source_version.clone(),
        mgr.fingerprint_engine.clone(),
        unit.marker_refresh_id.is_some(),
        unit.reuse_fingerprint_cache,
    )
    .await;

    // Marker publication is part of this probe: a store failure is reported, never dropped.
    if unit.marker_refresh_id.is_none()
        && matches!(
            &outcome,
            super::stages::FingerprintRunOutcome::Complete
                | super::stages::FingerprintRunOutcome::Partial { .. }
        )
        && let Err(error) = super::markers::compare_and_store_markers(mgr, unit)
    {
        tracing::error!(
            %error,
            job_id = ?job_id,
            media_id = %unit.row.file.media_id,
            ledger_id = %unit.row.file.file_id,
            "【片头片尾】标记发布失败"
        );
        outcome = super::stages::FingerprintRunOutcome::Failed {
            error_kind: "marker_store_error".into(),
            error: error.to_string(),
        };
    }

    match &outcome {
        super::stages::FingerprintRunOutcome::Complete => {
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.file.media_id,
                ledger_id = %unit.row.file.file_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【声纹】低优先级声纹处理完成"
            );
        }
        super::stages::FingerprintRunOutcome::Partial { .. } => {
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.file.media_id,
                ledger_id = %unit.row.file.file_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【声纹】低优先级声纹部分处理完成（片头就绪，片尾待重试）"
            );
        }
        super::stages::FingerprintRunOutcome::Failed { error, .. } => {
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.file.media_id,
                ledger_id = %unit.row.file.file_id,
                error = %error,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【声纹】后台声纹处理失败；已提取的媒体信息仍保留"
            );
        }
        super::stages::FingerprintRunOutcome::Cancelled { reason } => {
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.file.media_id,
                ledger_id = %unit.row.file.file_id,
                reason = %reason,
                "【声纹】声纹处理已取消"
            );
        }
    }
    outcome
}

async fn probe_adaptive_season(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    started_at: Instant,
) -> Option<bool> {
    let job_id = unit.job_id.clone();
    if unit.marker_refresh_id.is_some() {
        tracing::info!(
            job_id = ?job_id,
            media_id = %unit.row.file.media_id,
            season = projected_season(&unit.row),
            episode = projected_episode(&unit.row),
            ledger_id = %unit.row.file.file_id,
            "【自适应声纹】刷新任务由整季管道统一执行，单集单元跳过重复的旧全量采集"
        );
        return Some(true);
    }

    let season = projected_season(&unit.row);
    let season_units: Vec<ProbeUnit> = {
        let store = mgr.store.lock();
        let rows = store.list_file_coverage().unwrap_or_default();
        rows.into_iter()
            .filter(|r| r.file.media_id == unit.row.file.media_id && projected_season(r) == season)
            .map(|r| {
                let mut u = unit.clone();
                u.row = r;
                u
            })
            .collect()
    };

    if season_units.len() >= 2 {
        let job_id_str = job_id
            .clone()
            .unwrap_or_else(|| "adaptive-live".to_string());
        tracing::info!(
            job_id = ?job_id,
            media_id = %unit.row.file.media_id,
            season,
            units = season_units.len(),
            "【自适应声纹】检测到多集剧集，启动整季自适应采样流程"
        );
        match super::season::run_adaptive_season_pipeline(
            mgr,
            &job_id_str,
            unit.row.file.media_id,
            season,
            &season_units,
        )
        .await
        {
            Ok(replacement) => {
                let chapter_updates_clone = replacement.chapter_updates.clone();
                let markers_len = replacement.markers.len();
                let publication = {
                    let store = mgr.store.lock();
                    let rows = season_units
                        .iter()
                        .map(|unit| unit.row.clone())
                        .collect::<Vec<_>>();
                    super::markers::validate_publication_rows(&store, &rows)
                        .and_then(|()| store.replace_marker_results_batch_checked(&[replacement]))
                };
                if let Err(e) = publication {
                    if matches!(&e, crate::store::StoreError::Missing(reason) if crate::fingerprint_job::source::is_file_deleted(reason))
                    {
                        mgr.cancel_unit(unit, "file_deleted");
                        return Some(false);
                    }
                    tracing::error!(error = %e, "自适应整季标记替换失败");
                } else {
                    if let Some(service) = mgr.chapter_frames() {
                        crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                            &chapter_updates_clone,
                            &service,
                        );
                    }
                    tracing::info!(
                        job_id = ?job_id,
                        media_id = %unit.row.file.media_id,
                        season,
                        markers = markers_len,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "【自适应声纹】整季自适应处理完成，标记和章节已原子替换并触发场景帧生成"
                    );
                    return Some(true);
                }
            }
            Err(err) => {
                if crate::fingerprint_job::source::is_file_deleted(&err) {
                    mgr.cancel_unit(unit, "file_deleted");
                    return Some(false);
                }
                tracing::warn!(error = %err, "自适应整季处理失败，回退到普通全窗口采集");
            }
        }
    }
    None
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
    mgr.timings.record_media_info(unit.job_id.as_deref(), 0, true);
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

async fn probe_tracks_and_store(
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
    let probe_unit = unit.clone();
    let control = probe_control(mgr, unit);
    let tracks_result = tokio::task::spawn_blocking(move || {
        read_tracks_with_nfo(&probe_unit, started_at, &control)
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
    let (tracks, duration_ms) = tracks;
    let published = {
        let store = mgr.store.lock();
        if crate::fingerprint_job::source::ensure_source_available(&store, &unit.row).is_err() {
            return Err(());
        }
        publish_probe_tracks(&store, unit, &source_version, &tracks, duration_ms)
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

pub(super) fn probe_control(mgr: &ProbeManager, unit: &ProbeUnit) -> library::WorkControl {
    let owner = mgr
        .store
        .lock()
        .library_for_path_strict(std::path::Path::new(&unit.row.file.path), unit.kind)
        .ok()
        .flatten()
        .map(|library| library.id);
    let registry = mgr.library_work.lock().clone();
    owner
        .and_then(|id| registry.and_then(|registry| registry.active_control(&id)))
        .unwrap_or_default()
}

fn read_tracks_with_nfo(
    unit: &ProbeUnit,
    probe_started_at: Instant,
    control: &library::WorkControl,
) -> Option<(library::Tracks, Option<i64>)> {
    let tracks_path = PathBuf::from(&unit.row.file.path);
    let nfo_path = tracks_path.with_extension("nfo");
    let media_id = unit.row.file.media_id;
    let season = projected_season(&unit.row);
    let episode = projected_episode(&unit.row);
    let probe_ledger_id = unit.row.file.file_id.to_string();
    match library::probe_tracks_and_duration_controlled(&tracks_path, control) {
        Ok((tracks, duration_ms)) => {
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

pub(super) fn fingerprint_duration(
    mgr: &ProbeManager,
    path: &PathBuf,
    kind: MediaKind,
    force_fingerprint: bool,
) -> Option<u32> {
    let store = mgr.store.lock();
    let configured = store
        .library_for_path(path, kind)
        .ok()
        .flatten()
        .is_some_and(|library| library.enable_fingerprint);
    let enabled = kind == MediaKind::Tv && (force_fingerprint || configured);
    enabled.then(|| {
        store
            .get_scrape_config()
            .ok()
            .map(|c| c.effective.fingerprint_duration_secs)
            .unwrap_or(180)
    })
}

pub(super) async fn probe_fingerprint_and_store_detailed(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    fp_path: PathBuf,
    duration_secs: u32,
    media_duration_ms: Option<i64>,
    source_version: String,
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    require_complete_markers: bool,
    allow_cache_reuse: bool,
) -> super::stages::FingerprintRunOutcome {
    let ledger_id = unit.row.file.file_id.to_string();
    let started = Instant::now();
    let cached = match mgr.store.lock().get_fingerprint_cache(&ledger_id) {
        Ok(cache) => cache,
        Err(error) => {
            tracing::error!(%error, ledger_id, "【声纹】读取指纹缓存失败，回退到重新采集");
            None
        }
    };
    let outcome = match crate::fingerprint_job::capture_or_reuse_fingerprints(
        &fp_path,
        &source_version,
        duration_secs,
        media_duration_ms,
        cached,
        allow_cache_reuse,
        fingerprint_engine,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(
                %error,
                ledger_id,
                media_id = %unit.row.file.media_id,
                season = projected_season(&unit.row),
                episode = projected_episode(&unit.row),
                path = %fp_path.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "【声纹】音频指纹采集失败"
            );
            return super::stages::FingerprintRunOutcome::Failed {
                error_kind: super::stages::classify_fingerprint_error(&error).into(),
                error,
            };
        }
    };
    let publication = {
        let store = mgr.store.lock();
        super::markers::validate_publication_rows(&store, std::slice::from_ref(&unit.row))
            .and_then(|()| store.put_fingerprint_cache(&ledger_id, &outcome.cache))
    };
    if let Err(error) = publication {
        if matches!(&error, crate::store::StoreError::Missing(reason) if crate::fingerprint_job::source::is_file_deleted(reason))
        {
            mgr.cancel_unit(unit, "file_deleted");
            return super::stages::FingerprintRunOutcome::Cancelled {
                reason: "file_deleted".into(),
            };
        }
        tracing::error!(%error, ledger_id, "【声纹】版本化指纹缓存持久化失败");
        return super::stages::FingerprintRunOutcome::Failed {
            error_kind: "store_error".into(),
            error: error.to_string(),
        };
    }
    log_fingerprint_publication(
        unit,
        &ledger_id,
        &fp_path,
        &source_version,
        &outcome,
        started,
    );
    mgr.timings.record_fingerprint(
        unit.job_id.as_deref(),
        outcome.intro_cache_hit,
        outcome.outro_cache_hit,
        outcome.intro_elapsed_ms,
        outcome.outro_elapsed_ms,
    );

    let job_id = unit.job_id.clone().unwrap_or_else(|| "direct".to_string());
    super::stages::record_fingerprint_stages(
        mgr,
        &ledger_id,
        &job_id,
        &outcome.cache.cache_key,
        outcome.outro_error.as_deref(),
        mgr.clock.lock().now_ms(),
    );
    match outcome.outro_error {
        Some(error) => {
            tracing::warn!(
                %error,
                ledger_id,
                media_id = %unit.row.file.media_id,
                season = projected_season(&unit.row),
                episode = projected_episode(&unit.row),
                require_complete_markers,
                "【声纹】片尾指纹暂不可用，保留片头缓存并允许下次只重试片尾"
            );
            let error_kind = super::stages::classify_fingerprint_error(&error).to_string();
            if require_complete_markers {
                super::stages::FingerprintRunOutcome::Failed { error_kind, error }
            } else {
                super::stages::FingerprintRunOutcome::Partial { error_kind, error }
            }
        }
        None => super::stages::FingerprintRunOutcome::Complete,
    }
}

fn log_fingerprint_publication(
    unit: &ProbeUnit,
    ledger_id: &str,
    fp_path: &std::path::Path,
    source_version: &str,
    outcome: &crate::fingerprint_job::FingerprintCaptureOutcome,
    started: Instant,
) {
    tracing::info!(
        ledger_id,
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episode = projected_episode(&unit.row),
        path = %fp_path.display(),
        source_version,
        cache_key = outcome.cache.cache_key,
        intro_cache_hit = outcome.intro_cache_hit,
        outro_cache_hit = outcome.outro_cache_hit,
        intro_words = outcome.cache.intro.len(),
        outro_words = outcome.cache.outro.as_ref().map(Vec::len).unwrap_or_default(),
        intro_elapsed_ms = outcome.intro_elapsed_ms,
        outro_elapsed_ms = outcome.outro_elapsed_ms,
        media_duration_ms = ?outcome.cache.media_duration_ms,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "【声纹】版本化指纹缓存已持久化"
    );
}

/// Publish the probed stream facts for one file.
///
/// `Ok(false)` means the file row no longer carries the version this worker probed (a rescan
/// replaced the file while it was being probed), so the result describes bytes that are gone.
fn publish_probe_tracks(
    store: &crate::Store,
    unit: &ProbeUnit,
    source_version: &str,
    tracks: &library::Tracks,
    duration_ms: Option<i64>,
) -> Result<bool, crate::store::StoreError> {
    let Some(expected) = store.get_file_version(unit.row.file.file_id)? else {
        return Ok(false);
    };
    if expected.source_version != source_version && expected.source_version != "v1" {
        return Ok(false);
    }
    let (resolution, codec, hdr) = match tracks.video.as_ref() {
        Some(video) => (video.resolution(), video.codec.clone(), video.hdr()),
        None => (None, None, None),
    };
    let details = store::files::FileDetails {
        source_revision: expected.revision,
        source_version: expected.source_version.clone(),
        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
        video_json: tracks
            .video
            .as_ref()
            .and_then(|video| serde_json::to_string(video).ok()),
        audio_json: serde_json::to_string(&tracks.audio)?,
        subtitle_json: serde_json::to_string(&tracks.subtitles)?,
        chapters_json: None,
        fingerprint_json: None,
        outro_fingerprint_json: None,
        format_duration_ms: duration_ms,
    };
    let publication = store::library_write::probe::ProbePublication {
        expected: &expected,
        tracks_version: store::library_write::probe::CURRENT_TRACKS_VERSION,
        resolution: resolution.as_deref(),
        codec: codec.as_deref(),
        hdr: hdr.as_deref(),
        duration_ms,
        details: Some(&details),
    };
    store.publish_probe_result(unit.row.file.file_id, &publication, now_ms())
}
