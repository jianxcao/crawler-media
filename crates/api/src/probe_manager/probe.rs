use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use domain::MediaKind;
use marker::FingerprintEngine;

use super::{ProbeManager, ProbeUnit, single_episode, single_season};
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

#[allow(clippy::large_enum_variant)]
pub(super) enum MetadataOutcome {
    Complete,
    Fingerprint(FingerprintWork),
    Failed,
    Retry,
}

/// Finish mandatory media information before scheduling optional fingerprint work.
pub(super) async fn probe_metadata(mgr: &ProbeManager, unit: &ProbeUnit) -> MetadataOutcome {
    let started_at = Instant::now();
    let job_id = unit.job_id.clone();
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.file.media_id,
        season = ?single_season(&unit.row),
        episode = ?single_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        "【媒体信息】高优先级探测开始"
    );
    let outcome = probe_metadata_inner(mgr, unit).await;
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.file.media_id,
        season = ?single_season(&unit.row),
        episode = ?single_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        result = match &outcome {
            MetadataOutcome::Complete => "complete",
            MetadataOutcome::Fingerprint(_) => "fingerprint_ready",
            MetadataOutcome::Failed => "failed",
            MetadataOutcome::Retry => "retry",
        },
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体信息】高优先级探测结束"
    );
    outcome
}

async fn probe_metadata_inner(mgr: &ProbeManager, unit: &ProbeUnit) -> MetadataOutcome {
    let path = PathBuf::from(&unit.row.file.path);
    let source_path = path.clone();
    let source_version = match mgr
        .dispatch_work
        .run_to_completion(move || crate::fingerprint_job::current_source_version(&source_path))
        .await
    {
        Ok(version) => version,
        Err(error) => {
            tracing::error!(%error, "媒体信息源版本读取失败");
            return MetadataOutcome::Retry;
        }
    };
    let media_duration_ms =
        match super::tracks::probe_tracks_and_store(mgr, unit, &source_version).await {
            Ok(duration_ms) => duration_ms,
            Err(super::tracks::TrackProbeError::Failed) => return MetadataOutcome::Failed,
            Err(super::tracks::TrackProbeError::Retry) => return MetadataOutcome::Retry,
        };
    if unit.kind != MediaKind::Tv {
        return MetadataOutcome::Complete;
    }
    let library_id = unit.row.file.library_id.to_string();
    let force = unit.force_fingerprint;
    let duration = crate::http::store_wait::store_read_handle(
        mgr.store.clone(),
        crate::http::store_wait::STORE_READ_WAIT,
        move |store| fingerprint_duration_for_store(store, &library_id, MediaKind::Tv, force),
    )
    .await;
    let duration = match duration {
        Ok(duration) => duration,
        Err(error) => {
            tracing::warn!(?error, "声纹配置读取繁忙，保留探测失败结果供重试");
            return MetadataOutcome::Retry;
        }
    };
    let Some(duration_secs) = duration else {
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
        season = ?single_season(&unit.row),
        episode = ?single_episode(&unit.row),
        ledger_id = %unit.row.file.file_id,
        path = %unit.row.file.path.display(),
        marker_refresh = unit.marker_refresh_id.is_some(),
        refresh_id = ?unit.marker_refresh_id,
        "【声纹】媒体信息已写库，进入低优先级声纹队列"
    );
    // Check if adaptive sampling mode is enabled for TV episodes
    if let Err(error) = super::require_single_unit(&unit.row) {
        return super::stages::FingerprintRunOutcome::Failed {
            error_kind: "unit_time_mapping_unsupported".into(),
            error: error.to_string(),
        };
    }
    let sampling_mode = {
        let store = mgr.store.lock();
        crate::scrape_store::ScrapeStoreExt::get_scrape_config(&*store)
            .ok()
            .map(|c| c.effective.fingerprint_sampling_mode)
            .unwrap_or_else(|| "full_window".to_string())
    };

    if sampling_mode == "adaptive"
        && unit.kind == MediaKind::Tv
        && let Some(result) = probe_adaptive_season(mgr, unit, started_at).await
    {
        return if result {
            super::stages::FingerprintRunOutcome::Complete
        } else {
            super::stages::FingerprintRunOutcome::Failed {
                error_kind: "adaptive_failure".into(),
                error: "Adaptive season failed".into(),
            }
        };
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
            season = ?single_season(&unit.row),
            episode = ?single_episode(&unit.row),
            ledger_id = %unit.row.file.file_id,
            "【自适应声纹】刷新任务由整季管道统一执行，单集单元跳过重复的旧全量采集"
        );
        return Some(true);
    }

    let season = super::require_episode_coordinates(&unit.row).ok()?.0;
    let season_units: Vec<ProbeUnit> = {
        let store = mgr.store.lock();
        let rows = store.list_file_coverage().unwrap_or_default();
        rows.into_iter()
            .filter(|r| {
                r.file.media_id == unit.row.file.media_id && super::covers_season(r, season)
            })
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
                let rows = season_units
                    .iter()
                    .map(|unit| unit.row.clone())
                    .collect::<Vec<_>>();
                let members = super::markers::source_observations(&rows);
                let publication = members.and_then(|members| {
                    let store = mgr.store.lock();
                    super::markers::validate_active_unit(&store, unit)?;
                    super::markers::validate_season_rows(&store, &rows).and_then(|()| {
                        store.replace_marker_results_batch_checked(&[replacement], &members)
                    })
                });
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

#[cfg(test)]
pub(super) fn fingerprint_duration(
    mgr: &ProbeManager,
    library_id: &str,
    kind: MediaKind,
    force_fingerprint: bool,
) -> Option<u32> {
    let store = mgr.dispatch_store()?;
    fingerprint_duration_for_store(&store, library_id, kind, force_fingerprint)
}

fn fingerprint_duration_for_store(
    store: &crate::Store,
    library_id: &str,
    kind: MediaKind,
    force_fingerprint: bool,
) -> Option<u32> {
    let configured = store
        .get_library(library_id)
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
                season = ?single_season(&unit.row),
                episode = ?single_episode(&unit.row),
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
    let publication =
        super::markers::observe_rows(std::slice::from_ref(&unit.row)).and_then(|()| {
            let store = mgr.store.lock();
            super::markers::validate_active_unit(&store, unit)?;
            super::markers::validate_publication_rows(&store, std::slice::from_ref(&unit.row))
                .and_then(|()| {
                    store.put_fingerprint_cache_checked(
                        &ledger_id,
                        Some(&unit.row.file.version),
                        &outcome.cache,
                    )
                })
        });
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
    super::stages::log_fingerprint_publication(
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
    super::stages::capture_outcome(unit, outcome.outro_error, require_complete_markers)
}

/// Publish stream facts pinned to the queued version. `Ok(false)`: a rescan replaced it.
pub(super) fn publish_probe_tracks(
    store: &crate::Store,
    unit: &ProbeUnit,
    source_version: &str,
    tracks: &library::Tracks,
    duration_ms: Option<i64>,
) -> Result<bool, crate::store::StoreError> {
    let expected = unit.row.file.version.clone();
    if expected.source_version != source_version {
        tracing::warn!(
            ledger_id = %unit.row.file.file_id,
            queued = %expected.source_version,
            probed = %source_version,
            "探测结果的来源版本与入队版本不一致，拒绝发布"
        );
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
        hdr: if tracks.video.is_some() {
            store::library_write::probe::Observed::Value(hdr.as_deref())
        } else {
            store::library_write::probe::Observed::NotObserved
        },
        duration_ms,
        details: Some(&details),
    };
    store.publish_probe_result(unit.row.file.file_id, &publication, now_ms())
}
