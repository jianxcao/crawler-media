use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use domain::MediaKind;
use marker::FingerprintEngine;

use super::{ProbeManager, ProbeUnit};
use crate::scrape_store::ScrapeStoreExt;

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
    let job_id = mgr.active_probe_job_id(&unit.row.id.to_string());
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id = %unit.row.id,
        path = %unit.row.path,
        "【媒体信息】高优先级探测开始"
    );
    let outcome = probe_metadata_inner(mgr, unit).await;
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id = %unit.row.id,
        path = %unit.row.path,
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
    let path = PathBuf::from(&unit.row.path);
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
            media_id = %unit.row.media_id,
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
    let unit = &work.unit;
    let started_at = Instant::now();
    let job_id = unit.job_id.clone();
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id = %unit.row.id,
        path = %unit.row.path,
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

    if sampling_mode == "adaptive" && unit.kind == domain::MediaKind::Tv {
        if unit.marker_refresh_id.is_some() {
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id = %unit.row.id,
                "【自适应声纹】刷新任务由整季管道统一执行，单集单元跳过重复的旧全量采集"
            );
            return true;
        }

        let season = unit.row.season.unwrap_or(1);
        let season_units: Vec<ProbeUnit> = {
            let store = mgr.store.lock();
            let rows = store.list_ledger().unwrap_or_default();
            rows.into_iter()
                .filter(|r| r.media_id == unit.row.media_id && r.season.unwrap_or(1) == season)
                .map(|r| {
                    let mut u = unit.clone();
                    u.row = r;
                    u
                })
                .collect()
        };

        if season_units.len() >= 2 {
            let job_id_str = job_id.clone().unwrap_or_else(|| "adaptive-live".to_string());
            tracing::info!(
                job_id = ?job_id,
                media_id = %unit.row.media_id,
                season,
                units = season_units.len(),
                "【自适应声纹】检测到多集剧集，启动整季自适应采样流程"
            );
            match super::season::run_adaptive_season_pipeline(
                mgr,
                &job_id_str,
                unit.row.media_id,
                season,
                &season_units,
            )
            .await
            {
                Ok(replacement) => {
                    let chapter_updates_clone = replacement.chapter_updates.clone();
                    let markers_len = replacement.markers.len();
                    if let Err(e) = mgr.store.lock().replace_marker_results_batch(&[replacement]) {
                        tracing::error!(error = %e, "自适应整季标记替换失败");
                    } else {
                        {
                            let store = mgr.store.lock();
                            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                                &chapter_updates_clone,
                                &store,
                            );
                        }
                        tracing::info!(
                            job_id = ?job_id,
                            media_id = %unit.row.media_id,
                            season,
                            markers = markers_len,
                            elapsed_ms = started_at.elapsed().as_millis() as u64,
                            "【自适应声纹】整季自适应处理完成，标记和章节已原子替换并触发场景帧生成"
                        );
                        return true;
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "自适应整季处理失败，回退到普通全窗口采集");
                }
            }
        }
    }

    let path = PathBuf::from(&unit.row.path);
    if !probe_fingerprint_and_store(
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
    .await
    {
        tracing::info!(
            job_id = ?job_id,
            media_id = %unit.row.media_id,
            ledger_id = %unit.row.id,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【声纹】后台声纹处理失败；已提取的媒体信息仍保留"
        );
        return false;
    }
    if unit.marker_refresh_id.is_none() {
        if let Err(error) = super::markers::compare_and_store_markers(mgr, unit) {
            tracing::error!(
                %error,
                media_id = %unit.row.media_id,
                ledger_id = %unit.row.id,
                "【片头片尾】写入识别结果失败"
            );
            return false;
        }
    }
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        ledger_id = %unit.row.id,
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【声纹】低优先级声纹处理完成"
    );
    true
}

async fn probe_tracks_and_store(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    source_version: &str,
) -> Result<Option<i64>, ()> {
    let ledger_id = unit.row.id.to_string();
    let path = PathBuf::from(&unit.row.path);
    let started_at = Instant::now();
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id,
        path = %path.display(),
        refresh_id = ?unit.marker_refresh_id,
        "【媒体信息】流信息探测开始"
    );
    if unit.reuse_media_info_cache {
        let cached = {
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
                    Some(version.format_duration_ms)
                }
                (Err(error), _) | (_, Err(error)) => {
                    tracing::warn!(%error, ledger_id, "【媒体信息】读取版本缓存失败，回退到重新探测");
                    None
                }
                _ => None,
            }
        };
        if let Some(duration_ms) = cached {
            mgr.timings
                .record_media_info(unit.job_id.as_deref(), 0, true);
            tracing::info!(
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id,
                source_version,
                format_duration_ms = ?duration_ms,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "【媒体信息】版本未变化，复用已缓存流信息和媒体时长"
            );
            return Ok(duration_ms);
        }
    }
    let tracks_path = path.clone();
    let nfo_path = path.with_extension("nfo");
    let media_id = unit.row.media_id;
    let season = unit.row.season.unwrap_or(1);
    let episode = unit.row.episode.unwrap_or(1);
    let probe_ledger_id = ledger_id.clone();
    let probe_started_at = started_at;
    let tracks_result =
        tokio::task::spawn_blocking(move || {
            match library::probe_tracks_and_duration(&tracks_path) {
                Ok((tracks, duration_ms)) => {
                    let has_streams = tracks.video.is_some()
                        || !tracks.audio.is_empty()
                        || !tracks.subtitles.is_empty();
                    if has_streams {
                        if let Err(error) = library::write_streamdetails_into_nfo(
                            &nfo_path,
                            tracks.video.as_ref(),
                            &tracks.audio,
                            &tracks.subtitles,
                        ) {
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
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
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
    if let Err(error) = mgr.store.lock().put_file_meta_versioned(
        &ledger_id,
        &tracks,
        Some(source_version),
        duration_ms,
    ) {
        tracing::error!(
            %error,
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            ledger_id,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【媒体信息】流信息缓存写入失败"
        );
        return Err(());
    }
    // 探测得到真实主视频流后，顺带回写更新 ledger 行的 resolution, codec, hdr，保证台账与缓存一致
    if let Some(video) = tracks.video.as_ref() {
        let probed_res = video.resolution();
        let probed_codec = video.codec.as_deref();
        let probed_hdr = video.hdr();
        if probed_res.is_some() || probed_codec.is_some() || probed_hdr.is_some() {
            let store = mgr.store.lock();
            let _ = store.update_ledger_probe_quality(
                &ledger_id,
                probed_res.as_deref(),
                probed_codec,
                probed_hdr.as_deref(),
            );
        }
    }
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
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

async fn probe_fingerprint_and_store(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    fp_path: PathBuf,
    duration_secs: u32,
    media_duration_ms: Option<i64>,
    source_version: String,
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    require_complete_markers: bool,
    allow_cache_reuse: bool,
) -> bool {
    let ledger_id = unit.row.id.to_string();
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
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                path = %fp_path.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "【声纹】音频指纹采集失败"
            );
            return false;
        }
    };
    if let Err(error) = mgr
        .store
        .lock()
        .put_fingerprint_cache(&ledger_id, &outcome.cache)
    {
        tracing::error!(%error, ledger_id, "【声纹】版本化指纹缓存持久化失败");
        return false;
    }
    tracing::info!(
        ledger_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
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
    mgr.timings.record_fingerprint(
        unit.job_id.as_deref(),
        outcome.intro_cache_hit,
        outcome.outro_cache_hit,
        outcome.intro_elapsed_ms,
        outcome.outro_elapsed_ms,
    );
    if let Some(error) = outcome.outro_error {
        tracing::warn!(
            %error,
            ledger_id,
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            require_complete_markers,
            "【声纹】片尾指纹暂不可用，保留片头缓存并允许下次只重试片尾"
        );
        return !require_complete_markers;
    }
    true
}
