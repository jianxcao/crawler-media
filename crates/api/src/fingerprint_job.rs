//! Blocking audio fingerprint analysis for TV seasons.
//!
//! 指纹采集（远程拉音频）与比对分离：探测任务负责逐集采集并缓存指纹，
//! [`analyze_fingerprints`] 只用缓存指纹做比对，不再重复拉流。

use domain::{FileCoverage, MediaId};
use marker::{
    ChromaprintEngine, FingerprintEngine, MAX_MATCH_DURATION_SECS, MIN_MATCH_DURATION_SECS,
    extract_audio_fingerprint_at_with, match_episodes_fingerprints_with,
};

use crate::store::StoredMediaMarker;

pub mod adaptive;
mod cache;
mod cache_key;
pub(crate) mod source;

pub use adaptive::{
    AdaptiveCaptureContext, CaptureGate, CapturePolicy, EpisodeCaptureRequest, EpisodeDetection,
    capture_episode_adaptive,
};
pub use cache::{FingerprintCaptureOutcome, capture_or_reuse_fingerprints, current_source_version};
pub use cache_key::{
    FingerprintCaptureProfile, analysis_policy_key, capture_profile_key, fingerprint_cache_key,
    media_source_version,
};

/// 单集载体约定：一次比对只处理一个单元；多单元覆盖事实取首个单元并留下告警
/// （不静默丢事实）。整文件单元的覆盖事实在这里读作「无单元」。
fn selected_unit(coverage: &FileCoverage) -> Option<domain::UnitKey> {
    let adopted = coverage.units.first().copied();
    if coverage.units.len() > 1 {
        tracing::warn!(
            file_id = %coverage.file.file_id,
            units = coverage.units.len(),
            "声纹比对收到多单元覆盖事实，只按首个单元处理"
        );
    }
    adopted
}

/// 旧行模型 `episode: Option<u32>` 上退化默认 `unwrap_or(1)` 的等价物：
/// 整文件单元（`UnitKey::WHOLE`）复现旧 `None` → 1。
fn episode_or_one(coverage: &FileCoverage) -> u32 {
    selected_unit(coverage)
        .and_then(|unit| unit.episode_coordinates())
        .map_or(1, |(_, episode)| episode)
}

/// 从已采集的指纹集合中识别片头片尾：
/// `known_intro`: 各集的片头指纹 `(coverage, fp)`
/// `known_outro`: 各集的片尾指纹 `(coverage, fp, base_offset_ms)`
pub fn analyze_fingerprints_with_outro(
    media_id: MediaId,
    season: u32,
    known_intro: Vec<(FileCoverage, Vec<u32>)>,
    known_outro: Vec<(FileCoverage, Vec<u32>, i64)>,
) -> Vec<StoredMediaMarker> {
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
) -> Vec<StoredMediaMarker> {
    if known_intro.len() < 2 && known_outro.len() < 2 {
        return Vec::new();
    }

    let mut marker_map: std::collections::HashMap<u32, StoredMediaMarker> =
        std::collections::HashMap::new();

    if known_intro.len() >= 2 {
        let input_pairs: Vec<(u32, Vec<u32>)> = known_intro
            .iter()
            .map(|(row, fp)| (episode_or_one(row), fp.clone()))
            .collect();
        for d in match_episodes_fingerprints_with(
            engine,
            &input_pairs,
            MIN_MATCH_DURATION_SECS,
            MAX_MATCH_DURATION_SECS,
        ) {
            marker_map.insert(
                d.episode,
                StoredMediaMarker {
                    media_id,
                    season,
                    episode: d.episode,
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

    if known_outro.len() >= 2 {
        let input_triplets: Vec<(u32, Vec<u32>, i64)> = known_outro
            .iter()
            .map(|(row, fp, offset)| (episode_or_one(row), fp.clone(), *offset))
            .collect();
        for d in marker::match_episodes_outros_with(
            engine,
            &input_triplets,
            MIN_MATCH_DURATION_SECS,
            MAX_MATCH_DURATION_SECS,
        ) {
            let entry = marker_map
                .entry(d.episode)
                .or_insert_with(|| StoredMediaMarker {
                    media_id,
                    season,
                    episode: d.episode,
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

    marker_map.into_values().collect()
}

pub fn analyze_fingerprints(
    media_id: MediaId,
    season: u32,
    known: Vec<(FileCoverage, Vec<u32>)>,
) -> Vec<StoredMediaMarker> {
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
) -> Result<Vec<StoredMediaMarker>, String> {
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
) -> Result<Vec<StoredMediaMarker>, String> {
    let mut missing_rows: Vec<&FileCoverage> = rows
        .iter()
        .filter(|row| row.season_or_one() == season)
        .filter(|row| !rows_with_markers.contains(&(row.season_or_one(), episode_or_one(row))))
        .collect();
    missing_rows.sort_by_key(|row| episode_or_one(row));

    // 每集只提取一次指纹（失败跳过，不因单集失败放弃整季）。
    let mut known: Vec<(FileCoverage, Vec<u32>)> = Vec::new();
    for row in &missing_rows {
        match extract_audio_fingerprint_at_with(
            engine,
            &row.file.path,
            0,
            fingerprint_duration_secs,
        ) {
            Ok(fp) => known.push(((*row).clone(), fp)),
            Err(error) => {
                tracing::warn!(
                    media_id = %media_id,
                    season,
                    episode = episode_or_one(row),
                    path = %row.file.path.display(),
                    error = %error,
                    "【片头片尾】声纹比对：第 {} 集音频提取失败，跳过该集",
                    episode_or_one(row)
                );
            }
        }
    }
    Ok(analyze_fingerprints_with_outro_engine(
        engine,
        media_id,
        season,
        known,
        Vec::new(),
    ))
}
