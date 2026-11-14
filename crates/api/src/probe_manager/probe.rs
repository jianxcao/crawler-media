use std::path::PathBuf;
use std::time::Instant;

use domain::MediaKind;

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
pub(super) async fn probe_metadata(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    control: &library::WorkControl,
) -> MetadataOutcome {
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
    let outcome = probe_metadata_inner(mgr, unit, control).await;
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

async fn probe_metadata_inner(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    control: &library::WorkControl,
) -> MetadataOutcome {
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
        match super::tracks::probe_tracks_and_store(mgr, unit, &source_version, control).await {
            Ok(duration_ms) => duration_ms,
            Err(super::tracks::TrackProbeError::Failed) => return MetadataOutcome::Failed,
            Err(super::tracks::TrackProbeError::Retry) => return MetadataOutcome::Retry,
        };
    if unit.kind != MediaKind::Tv {
        return MetadataOutcome::Complete;
    }
    let library_id = unit.row.file.library_id.to_string();
    let explicit_refresh = unit.marker_refresh_id.is_some();
    let duration = mgr
        .store_access()
        .read_async(move |store| {
            fingerprint_duration_for_store(store, &library_id, MediaKind::Tv, explicit_refresh)
        })
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
        return MetadataOutcome::Complete;
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
    control: &marker::WorkControl,
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
    let plan = match super::analysis::prepare_plan(mgr, work) {
        Ok(plan) => plan,
        Err(error @ (store::StoreError::Json(_) | store::StoreError::Invalid(_))) => {
            tracing::error!(%error,job_id=?unit.job_id,"持久采集计划无效，终止单元");
            return super::stages::FingerprintRunOutcome::Failed {
                error_kind: "invalid_capture_plan".into(),
                error: format!("invalid_capture_plan: {error}"),
            };
        }
        Err(error) => {
            tracing::warn!(%error,"无法持久化采集策略，保留单元");
            return super::stages::FingerprintRunOutcome::Retry;
        }
    };

    if plan.sampling_mode == "adaptive" && unit.kind == MediaKind::Tv {
        return super::analysis::capture_target(mgr, unit, &plan).await;
    }

    let path = PathBuf::from(&unit.row.file.path);
    let outcome = probe_fingerprint_and_store_detailed(
        mgr,
        unit,
        path,
        plan.policy.full_window_duration_secs,
        work.media_duration_ms,
        work.source_version.clone(),
        unit.marker_refresh_id.is_some(),
        unit.reuse_fingerprint_cache,
        control,
    )
    .await;

    log_fingerprint_outcome(unit, started_at, &outcome);
    outcome
}

pub(super) fn fingerprint_duration_for_store(
    store: &crate::Store,
    library_id: &str,
    kind: MediaKind,
    explicit_refresh: bool,
) -> Option<u32> {
    let configured = store
        .get_library(library_id)
        .ok()
        .flatten()
        .is_some_and(|library| {
            explicit_refresh || (library.detect_intros && library.enable_fingerprint)
        });
    let available = store.library_accepts_work(library_id).ok().unwrap_or(false);
    let enabled = kind == MediaKind::Tv && configured && available;
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
    require_complete_markers: bool,
    allow_cache_reuse: bool,
    control: &marker::WorkControl,
) -> super::stages::FingerprintRunOutcome {
    let ledger_id = unit.row.file.file_id.to_string();
    let started = Instant::now();
    let mut cached = match mgr.store.get_fingerprint_cache(&ledger_id) {
        Ok(cache) => cache,
        Err(error) if super::store_access::is_busy(&error) => {
            return super::stages::FingerprintRunOutcome::Retry;
        }
        Err(error) => {
            tracing::error!(%error, ledger_id, "【声纹】读取指纹缓存失败，回退到重新采集");
            None
        }
    };
    let allow_cache_reuse =
        capture_job_allows_reuse(&mut cached, allow_cache_reuse, unit.job_id.as_deref());
    let capture_source = mgr.capture_source(&unit.row.file);
    let mut outcome = match crate::fingerprint_job::capture_or_reuse_fingerprints_from_source(
        &fp_path,
        &source_version,
        duration_secs,
        media_duration_ms,
        cached,
        allow_cache_reuse,
        std::sync::Arc::new(
            crate::fingerprint_job::capture_measurement::CaptureMeasurement {
                engine: mgr.capture_engine.clone(),
                timings: mgr.timings.clone(),
                job_id: unit.job_id.clone(),
            },
        ),
        control,
        capture_source,
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
    if !outcome.intro_cache_hit {
        outcome.cache.intro.metrics.captured_job_id = unit.job_id.clone();
    }
    if !outcome.outro_cache_hit
        && let Some(tail) = &mut outcome.cache.outro
    {
        tail.metrics.captured_job_id = unit.job_id.clone();
    }
    if let Err(outcome) = publish_captured_fingerprints(mgr, unit, &ledger_id, &outcome.cache) {
        return outcome;
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
    let now = mgr.clock.lock().now_ms();
    if let Err(error) = super::stages::record_fingerprint_stages(
        mgr,
        &ledger_id,
        &job_id,
        &outcome.cache.cache_key,
        outcome.outro_error.as_deref(),
        now,
    ) {
        tracing::warn!(%error, ledger_id, "声纹阶段状态未保存，保留单元等待重试");
        return super::stages::FingerprintRunOutcome::Retry;
    }
    super::stages::capture_outcome(unit, outcome.outro_error, require_complete_markers)
}

fn capture_job_allows_reuse(
    cached: &mut Option<store::FingerprintCacheEntry>,
    allow_cache_reuse: bool,
    job_id: Option<&str>,
) -> bool {
    // An explicit recapture bypasses earlier jobs. A retry of this same durable
    // job may retain its successful head while recovering a missing tail.
    let same_job_head = job_id.filter(|id| !id.is_empty()).is_some_and(|job_id| {
        cached
            .as_ref()
            .is_some_and(|cache| cache.intro.metrics.captured_job_id.as_deref() == Some(job_id))
    });
    if !allow_cache_reuse
        && same_job_head
        && let Some(cache) = cached
    {
        cache.outro = cache
            .outro
            .take()
            .filter(|tail| tail.metrics.captured_job_id.as_deref() == job_id);
    }
    allow_cache_reuse || same_job_head
}

fn publish_captured_fingerprints(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    ledger_id: &str,
    cache: &store::FingerprintCacheEntry,
) -> Result<(), super::stages::FingerprintRunOutcome> {
    let publication = cache.prepare().and_then(|prepared| {
        super::markers::observe_rows(std::slice::from_ref(&unit.row)).and_then(|()| {
            mgr.store.library_write(|store| {
                super::markers::validate_active_unit(store, unit)?;
                super::markers::validate_publication_rows(store, std::slice::from_ref(&unit.row))
                    .and_then(|()| {
                        store.put_fingerprint_cache_checked(
                            ledger_id,
                            Some(&unit.row.file.version),
                            &prepared,
                        )
                    })
            })
        })
    });
    if let Err(error) = publication {
        if matches!(&error, crate::store::StoreError::Missing(reason) if crate::fingerprint_job::source::is_file_deleted(reason))
        {
            mgr.cancel_unit(unit, "file_deleted");
            return Err(super::stages::FingerprintRunOutcome::Cancelled {
                reason: "file_deleted".into(),
            });
        }
        tracing::error!(%error, ledger_id, "【声纹】版本化指纹缓存持久化失败");
        if super::store_access::is_busy(&error) {
            return Err(super::stages::FingerprintRunOutcome::Retry);
        }
        return Err(super::stages::FingerprintRunOutcome::Failed {
            error_kind: "store_error".into(),
            error: error.to_string(),
        });
    }
    Ok(())
}

/// Publish stream facts pinned to the queued version. `Ok(false)`: a rescan replaced it.
pub(super) fn publish_probe_tracks(
    store: &store::LibraryConnection<'_>,
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

fn log_fingerprint_outcome(
    unit: &ProbeUnit,
    started_at: Instant,
    outcome: &super::stages::FingerprintRunOutcome,
) {
    let job_id = unit.job_id.clone();
    match outcome {
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
        super::stages::FingerprintRunOutcome::Uncertain { reason } => {
            tracing::info!(reason, file_id = %unit.row.file.file_id, "声纹候选未决，保留当前版本标记，等待新证据或显式刷新");
        }
        super::stages::FingerprintRunOutcome::Retry => {
            tracing::warn!(job_id = ?job_id, "声纹写入繁忙，保留持久单元等待重试");
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
}
