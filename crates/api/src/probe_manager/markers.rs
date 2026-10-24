use std::collections::HashSet;
use std::time::Instant;

use domain::FileCoverage;
use marker::FINGERPRINT_ALGORITHM_VERSION;
use sha2::{Digest, Sha256};

use super::{ProbeManager, ProbeUnit};
use super::{projected_episode, projected_season};
use crate::Store;
use crate::fingerprint_job::fingerprint_cache_key;
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{MarkerResultReplacement, StoredMediaMarker};

struct SeasonFingerprintSamples {
    all_rows: Vec<FileCoverage>,
    season_rows: Vec<FileCoverage>,
    intros: Vec<(FileCoverage, Vec<u32>)>,
    outros: Vec<(FileCoverage, Vec<u32>, i64)>,
}

struct MarkerComparison {
    samples: SeasonFingerprintSamples,
    markers: Vec<StoredMediaMarker>,
    intro_sample_episodes: HashSet<u32>,
    outro_sample_episodes: HashSet<u32>,
    intro_count: usize,
    outro_count: usize,
    started_at: Instant,
}

pub(super) fn compare_and_store_markers(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<(), crate::store::StoreError> {
    if !unit.overwrite_markers && season_input_unchanged(mgr, unit)? {
        tracing::debug!(
            media_id = %unit.row.file.media_id,
            season = projected_season(&unit.row),
            "【片头片尾】声纹输入未变化，跳过整季比对"
        );
        return Ok(());
    }
    let comparison = compare_season_fingerprints(mgr, unit)?;
    let season = projected_season(&unit.row);
    if comparison.markers.is_empty() && !unit.overwrite_markers {
        remember_comparison(mgr, unit, &comparison.samples);
        tracing::warn!(
            media_id = %unit.row.file.media_id,
            season,
            intro_samples = comparison.intro_count,
            outro_samples = comparison.outro_count,
            elapsed_ms = comparison.started_at.elapsed().as_millis() as u64,
            "【片头片尾】当前声纹没有形成可用匹配，保留现有标记"
        );
        return Ok(());
    }
    if unit.overwrite_markers {
        let replacement = build_marker_replacement(mgr, unit, &comparison)?;
        let chapter_count = replacement.chapter_updates.len();
        let chapter_updates_clone = replacement.chapter_updates.clone();
        let members = source_observations(&comparison.samples.season_rows)?;
        {
            let store = mgr.store.lock();
            validate_publication_rows(&store, std::slice::from_ref(&unit.row))?;
            validate_season_rows(&store, &comparison.samples.season_rows)?;
            store.replace_marker_results_batch_checked(&[replacement], &members)?;
        }
        if let Some(service) = mgr.chapter_frames() {
            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                &chapter_updates_clone,
                &service,
            );
        }
        tracing::info!(
            media_id = %unit.row.file.media_id,
            season,
            replaced_markers = comparison.markers.len(),
            updated_chapter_caches = chapter_count,
            "【片头片尾】已原子替换旧标记和章节缓存，并触发场景帧后台提取"
        );
    } else {
        let members = source_observations(&comparison.samples.season_rows)?;
        let live_rows = rows_present_on_disk(&comparison.samples.all_rows)?;
        let updates = {
            let store = mgr.store.lock();
            validate_publication_rows(&store, std::slice::from_ref(&unit.row))?;
            validate_season_rows(&store, &comparison.samples.season_rows)?;
            persist_markers_checked(&store, &live_rows, &comparison.markers, false, &members)?
        };
        // Drop Store before enqueueing; the service re-reads it when it runs.
        if let Some(service) = mgr.chapter_frames() {
            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                &updates, &service,
            );
        }
    }
    if unit.overwrite_markers {
        log_unmatched_episodes(unit, &comparison);
    }
    remember_comparison(mgr, unit, &comparison.samples);
    log_comparison_complete(unit, &comparison);
    Ok(())
}

/// Call only while holding Store, immediately before publishing the computed result.
/// Ledger-only publication check; run [`observe_rows`] (outside the lock) first.
pub(super) fn validate_publication_rows(
    store: &Store,
    rows: &[FileCoverage],
) -> Result<(), crate::store::StoreError> {
    for row in rows {
        crate::fingerprint_job::source::ensure_source_recorded(store, row)
            .map_err(crate::store::StoreError::Missing)?;
    }
    Ok(())
}

/// Disk observation of publication rows. Must run without the store lock.
pub(super) fn observe_rows(rows: &[FileCoverage]) -> Result<(), crate::store::StoreError> {
    for row in rows {
        crate::fingerprint_job::source::observe_source(row)
            .map_err(crate::store::StoreError::Missing)?;
    }
    Ok(())
}

/// Rows whose source is still on disk; deleted ones are skipped, errors propagate.
fn rows_present_on_disk(
    rows: &[FileCoverage],
) -> Result<Vec<FileCoverage>, crate::store::StoreError> {
    let mut live = Vec::with_capacity(rows.len());
    for row in rows {
        match crate::fingerprint_job::source::observe_source(row) {
            Ok(()) => live.push(row.clone()),
            Err(reason) if crate::fingerprint_job::source::is_file_deleted(&reason) => {}
            Err(reason) => return Err(crate::store::StoreError::Missing(reason)),
        }
    }
    Ok(live)
}

/// Observe the versions that supplied the comparison, never a newly loaded version.
pub(super) fn source_observations(
    rows: &[FileCoverage],
) -> Result<Vec<crate::store::SourceObservation>, crate::store::StoreError> {
    observe_rows(rows)?;
    Ok(rows
        .iter()
        .map(|row| crate::store::SourceObservation {
            file_id: row.file.file_id,
            expected: row.file.version.clone(),
            presence: domain::FilePresence::Present,
        })
        .collect())
}

/// Every comparison input participates, including rows outside a partial job lease.
pub(super) fn validate_season_rows(
    store: &Store,
    rows: &[FileCoverage],
) -> Result<(), crate::store::StoreError> {
    validate_publication_rows(store, rows)?;
    let seasons = rows
        .iter()
        .map(|row| (row.file.media_id, projected_season(row)))
        .collect::<HashSet<_>>();
    for (media_id, season) in seasons {
        let expected = rows
            .iter()
            .filter(|row| row.file.media_id == media_id && projected_season(row) == season)
            .map(|row| row.file.file_id)
            .collect::<HashSet<_>>();
        let actual = store
            .file_coverage_for_media(media_id)?
            .into_iter()
            .filter(|row| projected_season(row) == season)
            .map(|row| row.file.file_id)
            .collect::<HashSet<_>>();
        if actual != expected {
            return Err(crate::store::StoreError::Missing(
                "file_deleted: comparison membership changed".into(),
            ));
        }
    }
    Ok(())
}

fn season_input_unchanged(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<bool, crate::store::StoreError> {
    let digest = season_digest(mgr, unit)?;
    let stored = mgr.store.lock().season_comparison_digest(
        &unit.row.file.media_id.to_string(),
        projected_season(&unit.row),
    )?;
    Ok(stored.as_deref() == Some(digest.as_str()))
}

fn remember_season_digest(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    samples: &SeasonFingerprintSamples,
) -> Result<(), crate::store::StoreError> {
    let digest = comparison_digest(samples);
    observe_rows(&samples.season_rows)?;
    let store = mgr.store.lock();
    validate_season_rows(&store, &samples.season_rows)?;
    store.save_season_comparison_digest(
        &unit.row.file.media_id.to_string(),
        projected_season(&unit.row),
        &digest,
        mgr.clock.lock().now_ms(),
    )
}

fn remember_comparison(mgr: &ProbeManager, unit: &ProbeUnit, samples: &SeasonFingerprintSamples) {
    if let Err(error) = remember_season_digest(mgr, unit, samples) {
        tracing::warn!(%error, media_id = %unit.row.file.media_id, season = projected_season(&unit.row),
            "【片头片尾】比对摘要未保存，下次重新比对");
    }
}

fn season_digest(mgr: &ProbeManager, unit: &ProbeUnit) -> Result<String, crate::store::StoreError> {
    let samples = load_season_samples(mgr, unit)?;
    Ok(comparison_digest(&samples))
}

fn comparison_digest(samples: &SeasonFingerprintSamples) -> String {
    let mut parts = Vec::new();
    for row in &samples.season_rows {
        parts.push(format!(
            "f:{}:{}:{}:{}:{}",
            row.file.file_id,
            row.file.version.revision,
            row.file.version.source_version,
            projected_season(row),
            projected_episode(row)
        ));
    }
    for (row, words) in &samples.intros {
        parts.push(format!(
            "i:{}:{}",
            row.file.file_id,
            fingerprint_digest(words)
        ));
    }
    for (row, words, offset) in &samples.outros {
        parts.push(format!(
            "o:{}:{}:{}",
            row.file.file_id,
            fingerprint_digest(words),
            offset
        ));
    }
    parts.sort();
    parts.join("|")
}

fn fingerprint_digest(words: &[u32]) -> String {
    let mut digest = Sha256::new();
    for word in words {
        digest.update(word.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn prepare_marker_replacement(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<MarkerResultReplacement, crate::store::StoreError> {
    let comparison = compare_season_fingerprints(mgr, unit)?;
    let replacement = build_marker_replacement(mgr, unit, &comparison)?;
    log_unmatched_episodes(unit, &comparison);
    Ok(replacement)
}

fn compare_season_fingerprints(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<MarkerComparison, crate::store::StoreError> {
    let started_at = Instant::now();
    let season = projected_season(&unit.row);
    let samples = load_season_samples(mgr, unit)?;
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season,
        episode_rows = samples.season_rows.len(),
        intro_samples = samples.intros.len(),
        outro_samples = samples.outros.len(),
        overwrite = unit.overwrite_markers,
        refresh_id = ?unit.marker_refresh_id,
        "【片头片尾】声纹比对开始"
    );
    let intro_sample_episodes = samples
        .intros
        .iter()
        .map(|(row, _)| projected_episode(row))
        .collect();
    let outro_sample_episodes = samples
        .outros
        .iter()
        .map(|(row, _, _)| projected_episode(row))
        .collect();
    let compare_started_at = Instant::now();
    mgr.comparison_runs
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let markers = if samples.intros.len() >= 2 || samples.outros.len() >= 2 {
        crate::fingerprint_job::analyze_fingerprints_with_outro_engine(
            mgr.fingerprint_engine.as_ref(),
            unit.row.file.media_id,
            season,
            samples.intros.clone(),
            samples.outros.clone(),
        )
    } else {
        Vec::new()
    };
    mgr.timings.record_comparison(
        unit.job_id.as_deref(),
        compare_started_at.elapsed().as_millis() as u64,
    );
    log_comparison_counts(unit, &samples, &markers, compare_started_at);
    log_match_details(unit, &samples.all_rows, &markers);
    Ok(MarkerComparison {
        intro_count: samples.intros.len(),
        outro_count: samples.outros.len(),
        samples,
        markers,
        intro_sample_episodes,
        outro_sample_episodes,
        started_at,
    })
}

fn load_season_samples(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<SeasonFingerprintSamples, crate::store::StoreError> {
    let store = mgr.store.lock();
    let all_rows = store.list_file_coverage()?;
    let season_rows: Vec<_> = all_rows
        .iter()
        .filter(|row| {
            row.file.media_id == unit.row.file.media_id
                && projected_season(row) == projected_season(&unit.row)
        })
        .cloned()
        .collect();
    let duration_secs = store
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.fingerprint_duration_secs)
        .unwrap_or(180) as i64;
    let mut intros = Vec::new();
    let mut outros = Vec::new();
    for row in &season_rows {
        let key = row.file.file_id.to_string();
        match store.get_fingerprint_cache(&key) {
            Ok(Some(cache)) => {
                // The final out-of-lock observation checks this pinned ledger version.
                let source_version = &row.file.version.source_version;
                let expected_key = fingerprint_cache_key(
                    &source_version,
                    cache.sample_duration_secs,
                    cache.media_duration_ms,
                );
                if cache.cache_key != expected_key
                    || cache.algorithm_version != FINGERPRINT_ALGORITHM_VERSION
                    || i64::from(cache.sample_duration_secs) != duration_secs
                {
                    tracing::warn!(
                        ledger_id = %key,
                        episode = projected_episode(row),
                        cache_key = %cache.cache_key,
                        expected_cache_key = %expected_key,
                        source_version = %source_version,
                        cached_algorithm = cache.algorithm_version,
                        expected_algorithm = FINGERPRINT_ALGORITHM_VERSION,
                        cached_sample_duration_secs = cache.sample_duration_secs,
                        expected_sample_duration_secs = duration_secs,
                        "【片头片尾】跳过身份不匹配的声纹缓存，需重新采集"
                    );
                    continue;
                }
                if !cache.intro.is_empty() {
                    intros.push((row.clone(), cache.intro));
                }
                if let (Some(fingerprint), Some(duration_ms)) =
                    (cache.outro, cache.media_duration_ms)
                {
                    let offset_ms =
                        (duration_ms - i64::from(cache.sample_duration_secs) * 1000).max(0);
                    outros.push((row.clone(), fingerprint, offset_ms));
                }
            }
            Ok(None) => {}
            Err(error) => tracing::error!(
                %error,
                ledger_id = %key,
                "【片头片尾】读取单集声纹缓存失败"
            ),
        }
    }
    Ok(SeasonFingerprintSamples {
        all_rows,
        season_rows,
        intros,
        outros,
    })
}

fn log_comparison_counts(
    unit: &ProbeUnit,
    samples: &SeasonFingerprintSamples,
    markers: &[StoredMediaMarker],
    compare_started_at: Instant,
) {
    let intro_markers = markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some() || marker.intro_end_ms.is_some())
        .count();
    let outro_markers = markers
        .iter()
        .filter(|marker| marker.outro_start_ms.is_some() || marker.outro_end_ms.is_some())
        .count();
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        intro_samples = samples.intros.len(),
        outro_samples = samples.outros.len(),
        matched_episodes = markers.len(),
        intro_markers,
        outro_markers,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = compare_started_at.elapsed().as_millis() as u64,
        "【片头片尾】声纹比对结束"
    );
}

fn log_match_details(unit: &ProbeUnit, rows: &[FileCoverage], markers: &[StoredMediaMarker]) {
    for marker in markers {
        let path = rows
            .iter()
            .find(|row| {
                row.file.media_id == marker.media_id
                    && projected_season(row) == marker.season
                    && projected_episode(row) == marker.episode
            })
            .map(|row| row.file.path.display().to_string())
            .unwrap_or_default();
        if unit.overwrite_markers {
            tracing::info!(
                media_id = %marker.media_id,
                season = marker.season,
                episode = marker.episode,
                path,
                intro_start_ms = ?marker.intro_start_ms,
                intro_end_ms = ?marker.intro_end_ms,
                outro_start_ms = ?marker.outro_start_ms,
                outro_end_ms = ?marker.outro_end_ms,
                "【片头片尾】单集新识别结果"
            );
        } else {
            tracing::debug!(
                media_id = %marker.media_id,
                season = marker.season,
                episode = marker.episode,
                path,
                intro_start_ms = ?marker.intro_start_ms,
                intro_end_ms = ?marker.intro_end_ms,
                outro_start_ms = ?marker.outro_start_ms,
                outro_end_ms = ?marker.outro_end_ms,
                "【片头片尾】单集新识别结果"
            );
        }
    }
}

fn build_marker_replacement(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    comparison: &MarkerComparison,
) -> Result<MarkerResultReplacement, crate::store::StoreError> {
    let season = projected_season(&unit.row);
    let store = mgr.store.lock();
    let mut chapter_updates = Vec::with_capacity(comparison.samples.season_rows.len());
    for row in &comparison.samples.season_rows {
        let matched = comparison
            .markers
            .iter()
            .find(|candidate| candidate.episode == projected_episode(row));
        let intro = matched.and_then(|candidate| {
            match (candidate.intro_start_ms, candidate.intro_end_ms) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                _ => None,
            }
        });
        let outro = matched.and_then(|candidate| {
            match (candidate.outro_start_ms, candidate.outro_end_ms) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                (Some(start), None) => Some((start, start + 60_000)),
                _ => None,
            }
        });
        let existing = store
            .get_cached_chapters(&row.file.file_id.to_string())?
            .unwrap_or_default();
        let complete = marker::build_complete_timeline_chapters(&existing, intro, outro, None);
        chapter_updates.push((row.file.file_id.to_string(), complete));
    }
    Ok(MarkerResultReplacement {
        media_id: unit.row.file.media_id,
        season,
        markers: comparison.markers.clone(),
        chapter_updates,
    })
}

fn log_unmatched_episodes(unit: &ProbeUnit, comparison: &MarkerComparison) {
    let matched = comparison
        .markers
        .iter()
        .map(|marker| marker.episode)
        .collect::<HashSet<_>>();
    for row in &comparison.samples.season_rows {
        let episode = projected_episode(row);
        if matched.contains(&episode) {
            continue;
        }
        tracing::info!(
            media_id = %unit.row.file.media_id,
            season = projected_season(&unit.row),
            episode,
            path = %row.file.path.display(),
            had_intro_sample = comparison.intro_sample_episodes.contains(&episode),
            had_outro_sample = comparison.outro_sample_episodes.contains(&episode),
            "【片头片尾】该集本轮未匹配到片头或片尾，刷新成功后移除旧结果"
        );
    }
}

fn log_comparison_complete(unit: &ProbeUnit, comparison: &MarkerComparison) {
    let intro_markers = comparison
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some() || marker.intro_end_ms.is_some())
        .count();
    let outro_markers = comparison
        .markers
        .iter()
        .filter(|marker| marker.outro_start_ms.is_some() || marker.outro_end_ms.is_some())
        .count();
    tracing::info!(
        media_id = %unit.row.file.media_id,
        season = projected_season(&unit.row),
        episodes = comparison.samples.season_rows.len(),
        matched_episodes = comparison.markers.len(),
        intro_markers,
        outro_markers,
        replaced = unit.overwrite_markers,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = comparison.started_at.elapsed().as_millis() as u64,
        "【片头片尾】声纹比对和标记写入完成"
    );
}

#[cfg(test)]
pub(super) fn persist_marker(
    store: &Store,
    rows: &[FileCoverage],
    marker: crate::store::StoredMediaMarker,
    overwrite: bool,
) -> Result<Vec<(String, Vec<library::ChapterMarker>)>, crate::store::StoreError> {
    let members = rows
        .iter()
        .map(|row| crate::store::SourceObservation {
            file_id: row.file.file_id,
            expected: row.file.version.clone(),
            presence: domain::FilePresence::Present,
        })
        .collect::<Vec<_>>();
    persist_markers_checked(store, rows, &[marker], overwrite, &members)
}

fn persist_markers_checked(
    store: &Store,
    rows: &[FileCoverage],
    markers: &[crate::store::StoredMediaMarker],
    overwrite: bool,
    members: &[crate::store::SourceObservation],
) -> Result<Vec<(String, Vec<library::ChapterMarker>)>, crate::store::StoreError> {
    let mut live = Vec::new();
    for row in rows.iter().filter(|row| {
        markers.iter().any(|marker| {
            row.file.media_id == marker.media_id
                && projected_season(row) == marker.season
                && projected_episode(row) == marker.episode
        })
    }) {
        match crate::fingerprint_job::source::ensure_source_recorded(store, row) {
            Ok(()) => live.push(row.clone()),
            Err(reason) if crate::fingerprint_job::source::is_file_deleted(&reason) => {
                tracing::info!(file_id = %row.file.file_id, %reason, "skip unavailable marker publication target");
            }
            Err(reason) => return Err(crate::store::StoreError::Missing(reason)),
        }
    }
    store.merge_marker_results_checked(&live, members, markers, overwrite)
}
