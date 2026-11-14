//! Blocking audio fingerprint analysis for TV seasons.
//!
//! 指纹采集（远程拉音频）与比对分离：探测任务负责逐集采集并缓存指纹，
//! [`analyze_fingerprints`] 只用缓存指纹做比对，不再重复拉流。

use domain::{FileCoverage, MediaId};
use marker::{
    ChromaprintEngine, FingerprintEngine, MAX_MATCH_DURATION_SECS, MIN_MATCH_DURATION_SECS,
    extract_audio_fingerprint_at_with, match_episodes_fingerprints_with,
};

use crate::store::StoredFileMarker;

pub mod adaptive;
pub mod benchmark;
mod cache;
mod cache_key;
pub(crate) mod capture_measurement;
pub mod measurement;
mod profile;
pub(crate) mod source;

pub use adaptive::{
    AdaptiveCaptureContext, CaptureGate, CapturePolicy, EpisodeCaptureRequest, EpisodeDetection,
    capture_episode_adaptive,
};
pub use cache::{
    FingerprintCaptureOutcome, capture_or_reuse_fingerprints,
    capture_or_reuse_fingerprints_from_source, current_source_version, observe_source,
};
pub use cache_key::{
    FingerprintCaptureProfile, analysis_policy_key, capture_profile_key, fingerprint_cache_key,
    fingerprint_cache_key_with_profile, media_source_version,
};
pub use profile::{capture_source, profile_for_source};
pub(crate) use profile::{key_for_file, reusable_capture, source_for_file_selected};

fn episode_number(coverage: &FileCoverage) -> Result<u32, String> {
    crate::probe_manager::require_episode_coordinates(coverage)
        .map(|(_, episode)| episode)
        .map_err(|error| error.to_string())
}

fn validate_episode_inputs<'a>(
    media_id: MediaId,
    season: u32,
    rows: impl Iterator<Item = &'a FileCoverage>,
) -> Result<(), String> {
    for row in rows {
        let (row_season, _) = crate::probe_manager::require_episode_coordinates(row)
            .map_err(|error| error.to_string())?;
        if row.file.media_id != media_id || row_season != season {
            return Err(
                "fingerprint_unit_mismatch: input belongs to another Media or season".into(),
            );
        }
    }
    Ok(())
}

/// 从已采集的指纹集合中识别片头片尾：
/// `known_intro`: 各集的片头指纹 `(coverage, fp)`
/// `known_outro`: 各集的片尾指纹 `(coverage, fp, base_offset_ms)`
pub fn analyze_fingerprints_with_outro(
    media_id: MediaId,
    season: u32,
    known_intro: Vec<(FileCoverage, Vec<u32>)>,
    known_outro: Vec<(FileCoverage, Vec<u32>, i64)>,
) -> Result<Vec<StoredFileMarker>, String> {
    analyze_fingerprints_with_outro_engine(
        &ChromaprintEngine,
        media_id,
        season,
        known_intro,
        known_outro,
    )
}

pub fn analyze_fingerprints_with_outro_engine(
    engine: &dyn FingerprintEngine,
    media_id: MediaId,
    season: u32,
    known_intro: Vec<(FileCoverage, Vec<u32>)>,
    known_outro: Vec<(FileCoverage, Vec<u32>, i64)>,
) -> Result<Vec<StoredFileMarker>, String> {
    validate_episode_inputs(
        media_id,
        season,
        known_intro
            .iter()
            .map(|(row, _)| row)
            .chain(known_outro.iter().map(|(row, _, _)| row)),
    )?;
    if known_intro.len() < 2 && known_outro.len() < 2 {
        return Ok(Vec::new());
    }

    let mut marker_map = std::collections::HashMap::new();
    for (target, _) in &known_intro {
        let episode = episode_number(target)?;
        let mut references = std::collections::HashSet::new();
        let input_pairs = known_intro
            .iter()
            .filter_map(|(row, fp)| {
                let other = episode_number(row).ok()?;
                if row.file.file_id == target.file.file_id
                    || (other != episode && references.insert(other))
                {
                    Some((other, fp.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if let Some(d) = match_episodes_fingerprints_with(
            engine,
            &input_pairs,
            MIN_MATCH_DURATION_SECS,
            MAX_MATCH_DURATION_SECS,
        )
        .into_iter()
        .find(|d| d.episode == episode)
        {
            marker_map.insert(
                target.file.file_id,
                StoredFileMarker {
                    file_id: target.file.file_id,
                    version: target.file.version.clone(),
                    unit: target.units[0],
                    intro_start_ms: Some(d.intro_start_ms),
                    intro_end_ms: Some(d.intro_end_ms),
                    outro_start_ms: None,
                    outro_end_ms: None,
                    source: "fingerprint".into(),
                    locked: false,
                    updated_at: 0,
                },
            );
        }
    }
    for (target, _, _) in &known_outro {
        let episode = episode_number(target)?;
        let mut references = std::collections::HashSet::new();
        let input_triplets = known_outro
            .iter()
            .filter_map(|(row, fp, offset)| {
                let other = episode_number(row).ok()?;
                if row.file.file_id == target.file.file_id
                    || (other != episode && references.insert(other))
                {
                    Some((other, fp.clone(), *offset))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if let Some(d) = marker::match_episodes_outros_with(
            engine,
            &input_triplets,
            MIN_MATCH_DURATION_SECS,
            MAX_MATCH_DURATION_SECS,
        )
        .into_iter()
        .find(|d| d.episode == episode)
        {
            let entry = marker_map
                .entry(target.file.file_id)
                .or_insert_with(|| StoredFileMarker {
                    file_id: target.file.file_id,
                    version: target.file.version.clone(),
                    unit: target.units[0],
                    intro_start_ms: None,
                    intro_end_ms: None,
                    outro_start_ms: None,
                    outro_end_ms: None,
                    source: "fingerprint".into(),
                    locked: false,
                    updated_at: 0,
                });
            entry.outro_start_ms = Some(d.outro_start_ms);
            entry.outro_end_ms = Some(d.outro_end_ms);
        }
    }

    Ok(marker_map.into_values().collect())
}

pub fn analyze_fingerprints(
    media_id: MediaId,
    season: u32,
    known: Vec<(FileCoverage, Vec<u32>)>,
) -> Result<Vec<StoredFileMarker>, String> {
    analyze_fingerprints_with_outro(media_id, season, known, Vec::new())
}

/// Analyze missing episodes by extracting fingerprints on demand.
/// Callers persist returned markers. 仅供本地文件等廉价场景使用；
/// STRM 场景请走探测任务缓存指纹后调用 [`analyze_fingerprints`]。
pub fn analyze_season_fingerprints(
    media_id: MediaId,
    season: u32,
    rows: &[FileCoverage],
    rows_with_markers: &std::collections::HashSet<(u32, u32)>,
    fingerprint_duration_secs: u32,
) -> Result<Vec<StoredFileMarker>, String> {
    analyze_season_fingerprints_with_engine(
        &ChromaprintEngine,
        media_id,
        season,
        rows,
        rows_with_markers,
        fingerprint_duration_secs,
    )
}

pub fn analyze_season_fingerprints_with_engine(
    engine: &dyn FingerprintEngine,
    media_id: MediaId,
    season: u32,
    rows: &[FileCoverage],
    rows_with_markers: &std::collections::HashSet<(u32, u32)>,
    fingerprint_duration_secs: u32,
) -> Result<Vec<StoredFileMarker>, String> {
    let season_rows = rows
        .iter()
        .filter(|row| crate::probe_manager::covers_season(row, season))
        .collect::<Vec<_>>();
    validate_episode_inputs(media_id, season, season_rows.iter().copied())?;
    let mut missing_rows = Vec::new();
    for row in season_rows {
        let episode = episode_number(row)?;
        if !rows_with_markers.contains(&(season, episode)) {
            missing_rows.push((row, episode));
        }
    }
    missing_rows.sort_by_key(|(_, episode)| *episode);

    // 每集只提取一次指纹（失败跳过，不因单集失败放弃整季）。
    let mut known: Vec<(FileCoverage, Vec<u32>)> = Vec::new();
    for (row, episode) in &missing_rows {
        match extract_audio_fingerprint_at_with(
            engine,
            &row.file.path,
            0,
            fingerprint_duration_secs,
            &marker::WorkControl::default(),
        ) {
            Ok(fp) => known.push(((*row).clone(), fp)),
            Err(error) => {
                tracing::warn!(
                    media_id = %media_id,
                    season,
                    episode = episode,
                    path = %row.file.path.display(),
                    error = %error,
                    "【片头片尾】声纹比对：第 {} 集音频提取失败，跳过该集",
                    episode
                );
            }
        }
    }
    analyze_fingerprints_with_outro_engine(engine, media_id, season, known, Vec::new())
}
