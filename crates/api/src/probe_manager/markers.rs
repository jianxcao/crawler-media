use std::collections::HashSet;
use std::time::Instant;

use domain::LedgerRow;

use super::{ProbeManager, ProbeUnit};
use crate::Store;
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{MarkerResultReplacement, StoredMediaMarker};

struct SeasonFingerprintSamples {
    all_rows: Vec<LedgerRow>,
    season_rows: Vec<LedgerRow>,
    intros: Vec<(LedgerRow, Vec<u32>)>,
    outros: Vec<(LedgerRow, Vec<u32>, i64)>,
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
    let comparison = compare_season_fingerprints(mgr, unit)?;
    let season = unit.row.season.unwrap_or(1);
    if comparison.markers.is_empty() && !unit.overwrite_markers {
        tracing::warn!(
            media_id = %unit.row.media_id,
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
        mgr.store
            .lock()
            .replace_marker_results_batch(&[replacement])?;
        tracing::info!(
            media_id = %unit.row.media_id,
            season,
            replaced_markers = comparison.markers.len(),
            updated_chapter_caches = chapter_count,
            "【片头片尾】已原子替换旧标记和章节缓存"
        );
    } else {
        let store = mgr.store.lock();
        for marker in comparison.markers.iter().cloned() {
            persist_marker(&store, &comparison.samples.all_rows, marker, false)?;
        }
    }
    if unit.overwrite_markers {
        log_unmatched_episodes(unit, &comparison);
    }
    log_comparison_complete(unit, &comparison);
    Ok(())
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
    let season = unit.row.season.unwrap_or(1);
    let samples = load_season_samples(mgr, unit)?;
    tracing::info!(
        media_id = %unit.row.media_id,
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
        .map(|(row, _)| row.episode.unwrap_or(1))
        .collect();
    let outro_sample_episodes = samples
        .outros
        .iter()
        .map(|(row, _, _)| row.episode.unwrap_or(1))
        .collect();
    let compare_started_at = Instant::now();
    let markers = if samples.intros.len() >= 2 || samples.outros.len() >= 2 {
        crate::fingerprint_job::analyze_fingerprints_with_outro_engine(
            mgr.fingerprint_engine.as_ref(),
            unit.row.media_id,
            season,
            samples.intros.clone(),
            samples.outros.clone(),
        )
    } else {
        Vec::new()
    };
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
    let all_rows = store.list_ledger()?;
    let season_rows: Vec<_> = all_rows
        .iter()
        .filter(|row| {
            row.media_id == unit.row.media_id
                && row.season.unwrap_or(1) == unit.row.season.unwrap_or(1)
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
        let key = row.id.to_string();
        if let Ok(Some(fingerprint)) = store.get_fingerprint(&key) {
            intros.push((row.clone(), fingerprint));
        }
        if let Ok(Some(fingerprint)) = store.get_outro_fingerprint(&key) {
            let duration_ms = store
                .get_file_meta(&key)
                .ok()
                .flatten()
                .and_then(|meta| meta.video.and_then(|video| video.duration_secs))
                .map(|duration| (duration * 1000.0) as i64)
                .unwrap_or(0);
            let offset_ms = (duration_ms - duration_secs * 1000).max(0);
            outros.push((row.clone(), fingerprint, offset_ms));
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
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
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

fn log_match_details(unit: &ProbeUnit, rows: &[LedgerRow], markers: &[StoredMediaMarker]) {
    for marker in markers {
        let path = rows
            .iter()
            .find(|row| {
                row.media_id == marker.media_id
                    && row.season.unwrap_or(1) == marker.season
                    && row.episode.unwrap_or(1) == marker.episode
            })
            .map(|row| row.path.as_str())
            .unwrap_or("");
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
    let season = unit.row.season.unwrap_or(1);
    let store = mgr.store.lock();
    let mut chapter_updates = Vec::with_capacity(comparison.samples.season_rows.len());
    for row in &comparison.samples.season_rows {
        let matched = comparison
            .markers
            .iter()
            .find(|candidate| candidate.episode == row.episode.unwrap_or(1));
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
        let mut existing = store
            .get_cached_chapters(&row.id.to_string())?
            .unwrap_or_default();
        existing.retain(|chapter| {
            !matches!(
                chapter.marker_type,
                Some(
                    marker::MarkerType::IntroStart
                        | marker::MarkerType::IntroEnd
                        | marker::MarkerType::CreditsStart
                )
            )
        });
        let complete = marker::build_complete_timeline_chapters(&existing, intro, outro, None);
        chapter_updates.push((row.id.to_string(), complete));
    }
    Ok(MarkerResultReplacement {
        media_id: unit.row.media_id,
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
        let episode = row.episode.unwrap_or(1);
        if matched.contains(&episode) {
            continue;
        }
        tracing::info!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode,
            path = %row.path,
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
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
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

pub(super) fn persist_marker(
    store: &Store,
    rows: &[LedgerRow],
    mut marker: crate::store::StoredMediaMarker,
    overwrite_markers: bool,
) -> Result<(), crate::store::StoreError> {
    let started_at = Instant::now();
    if let Some(existing) =
        store.get_media_marker(marker.media_id, Some(marker.season), Some(marker.episode))?
    {
        if !overwrite_markers {
            if existing.locked {
                marker.intro_start_ms = existing.intro_start_ms;
                marker.intro_end_ms = existing.intro_end_ms;
                marker.outro_start_ms = existing.outro_start_ms;
                marker.outro_end_ms = existing.outro_end_ms;
                marker.source = existing.source;
            } else {
                marker.intro_start_ms = marker.intro_start_ms.or(existing.intro_start_ms);
                marker.intro_end_ms = marker.intro_end_ms.or(existing.intro_end_ms);
                marker.outro_start_ms = marker.outro_start_ms.or(existing.outro_start_ms);
                marker.outro_end_ms = marker.outro_end_ms.or(existing.outro_end_ms);
            }
        }
        marker.locked = existing.locked && !overwrite_markers;
    }
    store.put_media_marker(&marker)?;
    for row in rows.iter().filter(|row| {
        row.media_id == marker.media_id
            && row.season.unwrap_or(1) == marker.season
            && row.episode.unwrap_or(1) == marker.episode
    }) {
        let intro = match (marker.intro_start_ms, marker.intro_end_ms) {
            (Some(s), Some(e)) if e > s => Some((s, e)),
            _ => None,
        };
        let outro = match (marker.outro_start_ms, marker.outro_end_ms) {
            (Some(s), Some(e)) if e > s => Some((s, e)),
            (Some(s), None) => Some((s, s + 60_000)),
            _ => None,
        };
        let existing_chapters = store
            .get_cached_chapters(&row.id.to_string())?
            .unwrap_or_default();
        let complete =
            marker::build_complete_timeline_chapters(&existing_chapters, intro, outro, None);
        store.put_cached_chapters(&row.id.to_string(), &complete)?;
        tracing::debug!(
            media_id = %marker.media_id,
            season = marker.season,
            episode = marker.episode,
            ledger_id = %row.id,
            path = %row.path,
            intro = intro.is_some(),
            outro = outro.is_some(),
            chapter_count = complete.len(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【片头片尾】单集章节缓存写入结束"
        );
    }
    Ok(())
}
