use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use domain::MediaKind;
use marker::FingerprintEngine;

use super::{ProbeManager, ProbeUnit};
use crate::scrape_store::ScrapeStoreExt;

/// 执行一次完整探测：streamdetails + 声纹指纹 + 同季片头比对。
pub(super) async fn probe_one(mgr: &ProbeManager, unit: &ProbeUnit) -> bool {
    let started_at = Instant::now();
    let job_id = mgr.active_probe_job_id(&unit.row.id.to_string());
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id = %unit.row.id,
        path = %unit.row.path,
        forced = unit.force_fingerprint,
        marker_refresh = unit.marker_refresh_id.is_some(),
        refresh_id = ?unit.marker_refresh_id,
        "【媒体探测】单集探测开始"
    );
    let succeeded = probe_one_inner(mgr, unit).await;
    tracing::info!(
        job_id = ?job_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id = %unit.row.id,
        path = %unit.row.path,
        refresh_id = ?unit.marker_refresh_id,
        succeeded,
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体探测】单集探测完成"
    );
    succeeded
}

async fn probe_one_inner(mgr: &ProbeManager, unit: &ProbeUnit) -> bool {
    let ledger_id = unit.row.id.to_string();
    let path = PathBuf::from(&unit.row.path);
    if !probe_tracks_and_store(mgr, unit).await {
        mgr.mark_failed(&ledger_id);
        return false;
    }
    if unit.kind != MediaKind::Tv {
        return true;
    }
    let Some(duration_secs) = fingerprint_duration(mgr, &path, unit.kind, unit.force_fingerprint)
    else {
        tracing::info!(
            media_id = %unit.row.media_id,
            path = %path.display(),
            forced = unit.force_fingerprint,
            "【声纹】媒体库未启用声纹比对或文件不属于可识别媒体库，跳过声纹生成"
        );
        return !unit.force_fingerprint;
    };
    if !probe_fingerprint_and_store(
        mgr,
        unit,
        path,
        duration_secs,
        mgr.fingerprint_engine.clone(),
        unit.marker_refresh_id.is_some(),
    )
    .await
    {
        mgr.mark_failed(&ledger_id);
        return false;
    }
    if unit.marker_refresh_id.is_none() {
        if let Err(error) = super::markers::compare_and_store_markers(mgr, unit) {
            tracing::error!(
                %error,
                media_id = %unit.row.media_id,
                ledger_id,
                "【片头片尾】写入识别结果失败"
            );
            return false;
        }
    }
    true
}

async fn probe_tracks_and_store(mgr: &ProbeManager, unit: &ProbeUnit) -> bool {
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
    let tracks_path = path.clone();
    let nfo_path = path.with_extension("nfo");
    let media_id = unit.row.media_id;
    let season = unit.row.season.unwrap_or(1);
    let episode = unit.row.episode.unwrap_or(1);
    let probe_ledger_id = ledger_id.clone();
    let probe_started_at = started_at;
    let tracks_result =
        tokio::task::spawn_blocking(move || match library::probe_tracks(&tracks_path) {
            Ok(tracks) => {
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
                Some(tracks)
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
        })
        .await;
    let tracks = match tracks_result {
        Ok(Some(tracks)) => tracks,
        Ok(None) => return false,
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
            return false;
        }
    };
    if let Err(error) = mgr.store.lock().put_file_meta(&ledger_id, &tracks) {
        tracing::error!(
            %error,
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            ledger_id,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【媒体信息】流信息缓存写入失败"
        );
        return false;
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
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "【媒体信息】流信息探测与缓存完成"
    );
    true
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
        .is_some_and(|lib| lib.detect_intros && lib.enable_fingerprint);
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
    fingerprint_engine: Arc<dyn FingerprintEngine>,
    require_complete_markers: bool,
) -> bool {
    let ledger_id = unit.row.id.to_string();
    let started = Instant::now();
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id,
        path = %fp_path.display(),
        refresh_id = ?unit.marker_refresh_id,
        sample_duration_secs = duration_secs,
        marker_refresh = require_complete_markers,
        "【声纹】开始生成片头声纹"
    );
    let path_clone = fp_path.clone();
    let engine = fingerprint_engine.clone();
    let fingerprint = tokio::task::spawn_blocking(move || {
        marker::extract_audio_fingerprint_at_with(engine.as_ref(), &path_clone, 0, duration_secs)
    })
    .await;
    let fp = match fingerprint {
        Ok(Ok(fingerprint)) => fingerprint,
        Ok(Err(error)) => {
            tracing::error!(
                %error,
                ledger_id,
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                path = %fp_path.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "【声纹】片头声纹生成失败"
            );
            return false;
        }
        Err(error) => {
            tracing::error!(
                %error,
                ledger_id,
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                path = %fp_path.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "【声纹】片头声纹生成任务异常退出"
            );
            return false;
        }
    };
    if fp.is_empty() {
        tracing::error!(
            ledger_id,
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            path = %fp_path.display(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "【声纹】片头声纹为空，拒绝写入"
        );
        return false;
    }
    if let Err(error) = mgr.store.lock().put_fingerprint(&ledger_id, Some(&fp)) {
        tracing::error!(
            %error,
            ledger_id,
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            path = %fp_path.display(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "【声纹】片头声纹缓存写入失败"
        );
        return false;
    }
    tracing::info!(
        ledger_id,
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        path = %fp_path.display(),
        fingerprint_words = fp.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "【声纹】片头声纹生成成功"
    );

    // 探测视频总时长，并提取倒数 duration_secs 的片尾音频指纹。
    let duration_started = Instant::now();
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id,
        path = %fp_path.display(),
        "【声纹】开始探测媒体总时长以定位片尾采样窗口"
    );
    let duration_path = fp_path.clone();
    let duration_result =
        tokio::task::spawn_blocking(move || library::probe_duration(&duration_path)).await;
    let duration_ms = match duration_result {
        Ok(Some(duration)) if duration > 0 => {
            tracing::info!(
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id,
                duration_ms = duration,
                duration_secs = duration as f64 / 1000.0,
                elapsed_ms = duration_started.elapsed().as_millis() as u64,
                "【声纹】媒体总时长探测成功"
            );
            Some(duration)
        }
        Ok(duration) => {
            tracing::warn!(
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id,
                duration_ms = ?duration,
                elapsed_ms = duration_started.elapsed().as_millis() as u64,
                "【声纹】媒体总时长无效，无法定位片尾采样窗口"
            );
            None
        }
        Err(error) => {
            tracing::error!(
                %error,
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id,
                path = %fp_path.display(),
                elapsed_ms = duration_started.elapsed().as_millis() as u64,
                "【声纹】媒体总时长探测任务异常退出"
            );
            None
        }
    };
    let Some(total_ms) = duration_ms else {
        tracing::info!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            ledger_id,
            intro_words = fp.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            complete = false,
            "【声纹】本集声纹采集结束：片头已缓存，片尾因缺少媒体时长未采集"
        );
        return !require_complete_markers;
    };

    let total_secs = (total_ms / 1000) as u32;
    let required_secs = duration_secs.saturating_add(30);
    let outro_succeeded = if total_secs > required_secs {
        let outro_start_secs = total_secs.saturating_sub(duration_secs);
        let outro_started = Instant::now();
        tracing::info!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            ledger_id,
            path = %fp_path.display(),
            total_duration_ms = total_ms,
            sample_start_secs = outro_start_secs,
            sample_duration_secs = duration_secs,
            "【声纹】开始生成片尾声纹"
        );
        let engine = fingerprint_engine.clone();
        let path_for_outro = fp_path.clone();
        let outro_result = tokio::task::spawn_blocking(move || {
            marker::extract_audio_fingerprint_at_with(
                engine.as_ref(),
                &path_for_outro,
                outro_start_secs,
                duration_secs,
            )
        })
        .await;
        match outro_result {
            Ok(Ok(outro_fp)) if !outro_fp.is_empty() => {
                if let Err(error) = mgr
                    .store
                    .lock()
                    .put_outro_fingerprint(&ledger_id, Some(&outro_fp))
                {
                    tracing::error!(
                        %error,
                        media_id = %unit.row.media_id,
                        season = unit.row.season.unwrap_or(1),
                        episode = unit.row.episode.unwrap_or(1),
                        ledger_id,
                        elapsed_ms = outro_started.elapsed().as_millis() as u64,
                        "【声纹】片尾声纹缓存写入失败"
                    );
                    false
                } else {
                    tracing::info!(
                        media_id = %unit.row.media_id,
                        season = unit.row.season.unwrap_or(1),
                        episode = unit.row.episode.unwrap_or(1),
                        ledger_id,
                        path = %fp_path.display(),
                        sample_start_secs = outro_start_secs,
                        sample_duration_secs = duration_secs,
                        fingerprint_words = outro_fp.len(),
                        elapsed_ms = outro_started.elapsed().as_millis() as u64,
                        "【声纹】片尾声纹生成并缓存成功"
                    );
                    true
                }
            }
            Ok(Ok(_)) => {
                tracing::error!(
                    media_id = %unit.row.media_id,
                    season = unit.row.season.unwrap_or(1),
                    episode = unit.row.episode.unwrap_or(1),
                    ledger_id,
                    elapsed_ms = outro_started.elapsed().as_millis() as u64,
                    "【声纹】片尾声纹为空，拒绝写入"
                );
                false
            }
            Ok(Err(error)) => {
                tracing::error!(
                    %error,
                    media_id = %unit.row.media_id,
                    season = unit.row.season.unwrap_or(1),
                    episode = unit.row.episode.unwrap_or(1),
                    ledger_id,
                    path = %fp_path.display(),
                    sample_start_secs = outro_start_secs,
                    sample_duration_secs = duration_secs,
                    elapsed_ms = outro_started.elapsed().as_millis() as u64,
                    "【声纹】片尾声纹生成失败"
                );
                false
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    media_id = %unit.row.media_id,
                    season = unit.row.season.unwrap_or(1),
                    episode = unit.row.episode.unwrap_or(1),
                    ledger_id,
                    path = %fp_path.display(),
                    elapsed_ms = outro_started.elapsed().as_millis() as u64,
                    "【声纹】片尾声纹生成任务异常退出"
                );
                false
            }
        }
    } else {
        if let Err(error) = mgr.store.lock().put_outro_fingerprint(&ledger_id, None) {
            tracing::error!(
                %error,
                media_id = %unit.row.media_id,
                season = unit.row.season.unwrap_or(1),
                episode = unit.row.episode.unwrap_or(1),
                ledger_id,
                total_secs,
                required_secs,
                "【声纹】短视频清除旧片尾声纹缓存失败"
            );
            return false;
        }
        tracing::info!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode = unit.row.episode.unwrap_or(1),
            ledger_id,
            total_secs,
            required_secs,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "【声纹】视频短于独立片尾采样条件，已清除旧片尾指纹"
        );
        true
    };

    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episode = unit.row.episode.unwrap_or(1),
        ledger_id,
        total_duration_ms = total_ms,
        intro_words = fp.len(),
        outro_succeeded,
        marker_refresh = require_complete_markers,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "【声纹】单集片头片尾声纹采集结束"
    );
    outro_succeeded || !require_complete_markers
}
