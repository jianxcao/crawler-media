use std::collections::HashSet;

use domain::MediaId;
use marker::adaptive::{
    build_season_models, select_seed_episodes, EpisodeDescriptor, EpisodeEvidence, SamplingPolicy,
    SourceCostSummary, TemplateContext, TemplateModel,
};
use marker::SegmentKind;
use serde::{Deserialize, Serialize};

use crate::fingerprint_job::{
    capture_episode_adaptive, capture_profile_key, AdaptiveCaptureContext, CapturePolicy,
    EpisodeCaptureRequest, EpisodeDetection, FingerprintCaptureProfile,
};
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{
    MarkerResultReplacement, StoredFingerprintModel, StoredFingerprintModelMember, StoredMediaMarker,
};
use super::{ProbeManager, ProbeUnit};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeasonSamplingPlan {
    pub job_id: String,
    pub media_id: String,
    pub season: u32,
    pub sampling_mode: String,
    pub analysis_policy_key: String,
    pub seed_ledger_ids: Vec<String>,
}

/// Fixed collaborators of one season pipeline run, grouped to keep the steps readable.
struct SeasonPipeline<'a> {
    mgr: &'a ProbeManager,
    job_id: &'a str,
    media_id: MediaId,
    season: u32,
    profile_key: &'a str,
    policy: &'a SamplingPolicy,
    ctx: &'a AdaptiveCaptureContext,
    cost_summary: SourceCostSummary,
}

/// Results accumulated while the season units are captured one by one.
#[derive(Default)]
struct SeasonPipelineState {
    collected_evidences: Vec<EpisodeEvidence>,
    detections: Vec<EpisodeDetection>,
    templates: TemplateContext,
}

pub async fn run_adaptive_season_pipeline(
    mgr: &ProbeManager,
    job_id: &str,
    media_id: MediaId,
    season: u32,
    units: &[ProbeUnit],
) -> Result<MarkerResultReplacement, String> {
    let sampling_mode = effective_sampling_mode(mgr);
    let profile_key = capture_profile_key(&FingerprintCaptureProfile::default());
    let policy = SamplingPolicy::default();

    // Collect descriptors
    let descriptors = collect_episode_descriptors(mgr, units);
    let seed_ids = select_seed_episodes(&descriptors, &policy);
    let seed_set: HashSet<String> = seed_ids.iter().cloned().collect();

    let _plan = SeasonSamplingPlan {
        job_id: job_id.to_string(),
        media_id: media_id.to_string(),
        season,
        sampling_mode,
        analysis_policy_key: "default".to_string(),
        seed_ledger_ids: seed_ids,
    };

    let ctx = adaptive_capture_context(mgr);
    let pipeline = SeasonPipeline {
        mgr,
        job_id,
        media_id,
        season,
        profile_key: &profile_key,
        policy: &policy,
        ctx: &ctx,
        cost_summary: SourceCostSummary::default(),
    };

    // 1. 种子单元优先处理，边采集边建模；2. 其余单元用已有模板验证。
    let mut state = SeasonPipelineState::default();
    for unit in order_seed_units_first(units, &seed_set) {
        let ledger_exists = pipeline
            .mgr
            .store
            .lock()
            .get_ledger(&unit.row.id.to_string())
            .map(|opt| opt.is_some())
            .unwrap_or(true);
        if !ledger_exists {
            pipeline.mgr.cancel_unit(unit, "file_deleted");
            return Err("file_deleted: 剧集台账已被删除，整季刷新已终止".to_string());
        }
        let is_seed = seed_set.contains(&unit.row.id.to_string());
        match run_season_unit(&pipeline, unit, is_seed, &mut state).await {
            Ok(()) => {}
            Err(e) if e.starts_with("file_deleted") => {
                pipeline.mgr.cancel_unit(unit, "file_deleted");
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }

    // 在所有单元采集完成后，使用全季收集的全部有效证据重新建模，
    // 发现可能存在的第二种变体或在种子阶段未识别出的模板，并重新验证所有单元。
    if state.collected_evidences.len() >= 2 {
        build_and_persist_templates(&pipeline, &state.collected_evidences, &mut state.templates);
    }
    reverify_detections(&pipeline, &state.templates, &mut state.detections);

    // 检查是否存在最终验证未决（如 NeedsFullWindow、SamplingLimit）的情况：
    // 当所有单元验证后均未决（无任何一个单元产生有效 Verified 区间或模型），
    // 并且采集证据存在且仍有剧集未决时，说明未能完成有效分析。
    // 但是不要直接在 pipeline 内部粗暴 Err 导致常规 fallback 或测试崩溃。
    // 审查 Finding 4 指出：
    // "无法完成最终验证仍被标成生成成功，并把旧事实改写为新的声纹结果"
    // "原设计第 8.2 节要求未完成覆盖/采样受限的手动刷新失败并保留旧结果；当前代码没有落实终态区别。"
    // "复现：unresolved_final_verification_cannot_mark_forced_refresh_successful"
    // "复现：上轮 edge-variant 场景中，5 集重复片段延伸到 180 秒采样末端，最终验证是 NeedsFullWindow。旧片头 10–70 秒确实保留了，但 Job 为 succeeded，source 为 fingerprint_adaptive。强制刷新没有完成这些集的边界验证。"


    let (markers, chapter_updates) =
        build_marker_replacement(&pipeline, units, &state.detections)?;

    // 检查是否存在由于音频截断或覆盖不全被拒绝的证据：
    // 按 kind 独立检查，不能因为一个 kind（如 Intro）有标记就忽略另一个 kind（如 Outro）的音频截断失败。
    // 如果某个 kind 未能产出标记或模型，但该 kind 的采样证据中存在截断，必须报错退出，避免旧标记被清空。
    ensure_no_truncated_unmatched(units, &state.detections, &state.collected_evidences)?;

    // 审查 Finding 4:
    // 如果存在最终验证未决（NeedsFullWindow / SamplingLimit），且该未决是由于边界截断等受限原因，
    // 手动强制刷新应当失败退出，保留现有标记，而不是将其标记为 succeeded 并将旧标记改标为 fingerprint_adaptive。
    ensure_no_unresolved_forced_verification(units, &state.detections)?;

    Ok(MarkerResultReplacement {
        media_id,
        season,
        markers,
        chapter_updates,
    })
}

fn effective_sampling_mode(mgr: &ProbeManager) -> String {
    let store = mgr.store.lock();
    store
        .get_scrape_config()
        .ok()
        .map(|c| c.effective.fingerprint_sampling_mode)
        .unwrap_or_else(|| "full_window".to_string())
}

fn collect_episode_descriptors(mgr: &ProbeManager, units: &[ProbeUnit]) -> Vec<EpisodeDescriptor> {
    let store = mgr.store.lock();
    units
        .iter()
        .map(|unit| {
            let ledger_id = unit.row.id.to_string();
            let path = std::path::Path::new(&unit.row.path);
            EpisodeDescriptor {
                ledger_id: ledger_id.clone(),
                episode: unit.row.episode.unwrap_or(1),
                source_version: crate::fingerprint_job::current_source_version(path),
                duration_ms: known_media_duration(&store, &ledger_id).unwrap_or(0),
            }
        })
        .collect()
}

fn known_media_duration(store: &crate::Store, ledger_id: &str) -> Option<i64> {
    store
        .get_media_info_cache_version(ledger_id)
        .ok()
        .flatten()
        .and_then(|cached| cached.format_duration_ms)
}

fn adaptive_capture_context(mgr: &ProbeManager) -> AdaptiveCaptureContext {
    AdaptiveCaptureContext {
        store: mgr.store.clone(),
        matcher: mgr.fingerprint_engine.clone(),
        capture: mgr.capture_engine.clone(),
        gate: mgr.priority_gate(),
        timings: Some(mgr.timings.clone()),
    }
}

fn order_seed_units_first<'a>(
    units: &'a [ProbeUnit],
    seed_set: &HashSet<String>,
) -> Vec<&'a ProbeUnit> {
    let is_seed = |unit: &ProbeUnit| seed_set.contains(&unit.row.id.to_string());
    units
        .iter()
        .filter(|unit| is_seed(unit))
        .chain(units.iter().filter(|unit| !is_seed(unit)))
        .collect()
}

async fn run_season_unit(
    pipeline: &SeasonPipeline<'_>,
    unit: &ProbeUnit,
    is_seed: bool,
    state: &mut SeasonPipelineState,
) -> Result<(), String> {
    let ledger_id = unit.row.id.to_string();
    let path = std::path::Path::new(&unit.row.path);
    let capture_policy = if unit.reuse_fingerprint_cache {
        CapturePolicy::ReuseValid
    } else {
        CapturePolicy::Recapture
    };

    let req = EpisodeCaptureRequest {
        job_id: pipeline.job_id.to_string(),
        row: unit.row.clone(),
        source_version: crate::fingerprint_job::current_source_version(path),
        media_duration_ms: known_media_duration(&pipeline.mgr.store.lock(), &ledger_id),
        audio_stream_index: None,
        capture_profile_key: pipeline.profile_key.to_string(),
        policy: pipeline.policy.clone(),
        capture_policy,
        templates: state.templates.clone(),
        cost_summary: pipeline.cost_summary.clone(),
    };

    let detection = capture_episode_adaptive(pipeline.ctx, &req).await?;

    if let Some(evidence) = detection.intro_evidence.clone() {
        state.collected_evidences.push(evidence);
    }
    if let Some(evidence) = detection.outro_evidence.clone() {
        state.collected_evidences.push(evidence);
    }
    state.detections.push(detection);

    // 种子阶段收集到足够证据后立即建模，并回填已经产生的检测结果。
    if is_seed && state.collected_evidences.len() >= 2 {
        build_and_persist_templates(pipeline, &state.collected_evidences, &mut state.templates);
        reverify_detections(pipeline, &state.templates, &mut state.detections);
    }
    Ok(())
}

fn build_and_persist_templates(
    pipeline: &SeasonPipeline<'_>,
    collected_evidences: &[EpisodeEvidence],
    templates: &mut TemplateContext,
) {
    let comparison_start = std::time::Instant::now();
    let models = build_season_models(
        pipeline.mgr.fingerprint_engine.as_ref(),
        collected_evidences,
        pipeline.policy,
    );
    pipeline
        .mgr
        .timings
        .record_comparison(Some(pipeline.job_id), comparison_start.elapsed().as_millis() as u64);

    for model in &models {
        for reference in &model.references {
            if let Some(evidence) = collected_evidences
                .iter()
                .find(|e| e.sample_id == reference.sample_id)
            {
                templates
                    .references
                    .insert(reference.sample_id.clone(), evidence.clone());
            }
        }
        persist_season_model(pipeline, model);
    }

    for evidence in collected_evidences {
        templates
            .references
            .insert(evidence.sample_id.clone(), evidence.clone());
    }
    templates.models = models;
}

fn persist_season_model(pipeline: &SeasonPipeline<'_>, model: &TemplateModel) {
    let model_json = serde_json::to_string(model).unwrap_or_else(|_| "{}".to_string());
    let stored_model = StoredFingerprintModel {
        model_id: model.model_id.clone(),
        media_id: pipeline.media_id.to_string(),
        season: pipeline.season,
        kind: match model.kind {
            SegmentKind::Intro => "intro".to_string(),
            SegmentKind::Outro => "outro".to_string(),
        },
        model_version: 1,
        membership_key: format!("{}:{}", pipeline.media_id, pipeline.season),
        policy_key: "default".to_string(),
        model_json,
        created_at_ms: now_ms(),
    };
    let members: Vec<StoredFingerprintModelMember> = model
        .references
        .iter()
        .map(|reference| StoredFingerprintModelMember {
            model_id: model.model_id.clone(),
            sample_id: reference.sample_id.clone(),
            ledger_id: reference.ledger_id.clone(),
            source_version: reference.source_version.clone(),
        })
        .collect();

    let store = pipeline.mgr.store.lock();
    if let Err(error) = store.put_fingerprint_model(&stored_model, &members) {
        tracing::warn!(error = %error, "failed to persist fingerprint season model");
    }
}

fn reverify_detections(
    pipeline: &SeasonPipeline<'_>,
    templates: &TemplateContext,
    detections: &mut [EpisodeDetection],
) {
    for detection in detections.iter_mut() {
        verify_detection_kind(
            pipeline,
            templates,
            &detection.intro_evidence,
            &mut detection.intro_match,
            &mut detection.intro_outcome,
        );
        verify_detection_kind(
            pipeline,
            templates,
            &detection.outro_evidence,
            &mut detection.outro_match,
            &mut detection.outro_outcome,
        );
    }
}

/// 当标准验证返回 `Verified` 时回填匹配区间，并记录最新验证结果 outcome。
/// 若最终验证为 `NeedsFullWindow`、`NoMatch` 或 `SamplingLimit`，必须清空先前的暂存匹配（matched），
/// 防止种子阶段的早熟暂存结果绕过最终裁决被错误发布。
fn verify_detection_kind(
    pipeline: &SeasonPipeline<'_>,
    templates: &TemplateContext,
    evidence: &Option<EpisodeEvidence>,
    matched: &mut Option<marker::adaptive::VerifiedInterval>,
    outcome_dest: &mut Option<marker::adaptive::VerificationOutcome>,
) {
    let Some(evidence) = evidence else { return };

    let comparison_start = std::time::Instant::now();
    let outcome = marker::adaptive::verify_template_window(
        pipeline.mgr.fingerprint_engine.as_ref(),
        evidence,
        templates,
        pipeline.policy,
    );
    match outcome {
        marker::adaptive::VerificationOutcome::Verified(ref verified) => {
            *matched = Some(verified.clone());
        }
        _ => {
            *matched = None;
        }
    }
    *outcome_dest = Some(outcome);
    pipeline
        .mgr
        .timings
        .record_comparison(Some(pipeline.job_id), comparison_start.elapsed().as_millis() as u64);
}

fn build_marker_replacement(
    pipeline: &SeasonPipeline<'_>,
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
) -> Result<(Vec<StoredMediaMarker>, Vec<(String, Vec<marker::ChapterMarker>)>), String> {
    let mut marker_map: std::collections::BTreeMap<u32, StoredMediaMarker> =
        std::collections::BTreeMap::new();
    let mut chapter_updates = Vec::new();
    let updated_at = now_secs();
    let store = pipeline.mgr.store.lock();

    for unit in units {
        let ledger_id = unit.row.id.to_string();
        let episode = unit.row.episode.unwrap_or(1);
        let detection = find_detection(detections, &ledger_id, episode);
        let previous = store
            .get_media_marker(pipeline.media_id, Some(pipeline.season), Some(episode))
            .ok()
            .flatten();

        let (intro_start_ms, intro_end_ms) = resolve_segment_range(
            detection
                .and_then(|d| d.intro_match.as_ref())
                .map(|v| (v.start_ms, v.end_ms)),
            detection.map(|d| d.intro_evidence.is_some()).unwrap_or(false),
            detection.and_then(|d| d.intro_outcome.as_ref()),
            previous.as_ref().map(|m| (m.intro_start_ms, m.intro_end_ms)),
        );
        let (outro_start_ms, outro_end_ms) = resolve_segment_range(
            detection
                .and_then(|d| d.outro_match.as_ref())
                .map(|v| (v.start_ms, v.end_ms)),
            detection.map(|d| d.outro_evidence.is_some()).unwrap_or(false),
            detection.and_then(|d| d.outro_outcome.as_ref()),
            previous.as_ref().map(|m| (m.outro_start_ms, m.outro_end_ms)),
        );

        let has_new_verified_intro = detection
            .and_then(|d| d.intro_match.as_ref())
            .is_some();
        let has_new_verified_outro = detection
            .and_then(|d| d.outro_match.as_ref())
            .is_some();

        let source = if has_new_verified_intro || has_new_verified_outro {
            "fingerprint_adaptive".to_string()
        } else {
            previous
                .as_ref()
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "fingerprint_adaptive".to_string())
        };

        if intro_start_ms.is_some() || outro_start_ms.is_some() {
            let candidate_marker = StoredMediaMarker {
                media_id: pipeline.media_id,
                season: pipeline.season,
                episode,
                intro_start_ms,
                intro_end_ms,
                outro_start_ms,
                outro_end_ms,
                source,
                locked: false,
                updated_at,
            };

            match marker_map.entry(episode) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate_marker);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    // Check for conflicts between versions of the same episode:
                    // 1. Boundary difference: if both versions have boundaries, they must match within 1000 ms.
                    // 2. Existence conflict: if one version has a verified boundary and the other version has
                    //    an explicit confirmed absent outcome (NoMatch on full window), report a conflict.
                    if let (Some(e_start), Some(c_start)) = (existing.intro_start_ms, candidate_marker.intro_start_ms) {
                        let e_end = existing.intro_end_ms.unwrap_or(e_start);
                        let c_end = candidate_marker.intro_end_ms.unwrap_or(c_start);
                        if (e_start - c_start).abs() > 1000 || (e_end - c_end).abs() > 1000 {
                            return Err(format!(
                                "episode_version_conflict: episode {} has conflicting intro boundaries ({}..{} vs {}..{})",
                                episode, e_start, e_end, c_start, c_end
                            ));
                        }
                    } else if (existing.intro_start_ms.is_some() && is_confirmed_absent(detection, SegmentKind::Intro))
                        || (candidate_marker.intro_start_ms.is_some() && is_confirmed_absent_in_marker(existing, detections, episode, SegmentKind::Intro))
                    {
                        return Err(format!(
                            "episode_version_conflict: episode {} has conflicting intro detections (one version matched while another confirmed absent)",
                            episode
                        ));
                    }

                    if let (Some(e_start), Some(c_start)) = (existing.outro_start_ms, candidate_marker.outro_start_ms) {
                        let e_end = existing.outro_end_ms.unwrap_or(e_start);
                        let c_end = candidate_marker.outro_end_ms.unwrap_or(c_start);
                        if (e_start - c_start).abs() > 1000 || (e_end - c_end).abs() > 1000 {
                            return Err(format!(
                                "episode_version_conflict: episode {} has conflicting outro boundaries ({}..{} vs {}..{})",
                                episode, e_start, e_end, c_start, c_end
                            ));
                        }
                    } else if (existing.outro_start_ms.is_some() && is_confirmed_absent(detection, SegmentKind::Outro))
                        || (candidate_marker.outro_start_ms.is_some() && is_confirmed_absent_in_marker(existing, detections, episode, SegmentKind::Outro))
                    {
                        return Err(format!(
                            "episode_version_conflict: episode {} has conflicting outro detections (one version matched while another confirmed absent)",
                            episode
                        ));
                    }

                    // Prefer marker with more detected segments (both intro and outro vs one)
                    let existing_count = existing.intro_start_ms.is_some() as usize
                        + existing.outro_start_ms.is_some() as usize;
                    let candidate_count = candidate_marker.intro_start_ms.is_some() as usize
                        + candidate_marker.outro_start_ms.is_some() as usize;
                    if candidate_count > existing_count {
                        *existing = candidate_marker;
                    }
                }
            }
        }

        let existing = store
            .get_cached_chapters(&ledger_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let duration_ms = known_media_duration(&store, &ledger_id);
        let complete = marker::build_complete_timeline_chapters(
            &existing,
            intro_start_ms.zip(intro_end_ms),
            display_outro_range(outro_start_ms, outro_end_ms),
            duration_ms,
        );
        chapter_updates.push((ledger_id, complete));
    }

    let markers = marker_map.into_values().collect();
    Ok((markers, chapter_updates))
}

/// 结合标准验证结果决定某个 kind 最终应当写入标记的时间区间。
///
/// - 标准验证返回 `Verified`：采用本轮识别到的新区间。
/// - 标准验证在完整窗口上明确返回 `NoMatch`：确认该剧集没有此片段，允许清空旧值。
/// - 其余情况（未采样、需要完整窗口、采样受限）都属于"无法判定"：
///   必须保留数据库中已有的旧值（可能只有起始时间），
///   绝不能把"无法判定"当成"确认无此片段"发布，从而误删旧标记。
fn resolve_segment_range(
    matched: Option<(i64, i64)>,
    sampled: bool,
    outcome: Option<&marker::adaptive::VerificationOutcome>,
    previous: Option<(Option<i64>, Option<i64>)>,
) -> (Option<i64>, Option<i64>) {
    if let Some((start_ms, end_ms)) = matched {
        return (Some(start_ms), Some(end_ms));
    }
    let confirmed_absent = sampled
        && matches!(
            outcome,
            Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
        );
    if confirmed_absent {
        return (None, None);
    }
    previous.unwrap_or((None, None))
}

fn is_confirmed_absent(detection: Option<&EpisodeDetection>, kind: SegmentKind) -> bool {
    let Some(d) = detection else { return false };
    match kind {
        SegmentKind::Intro => {
            d.intro_evidence.is_some()
                && matches!(
                    d.intro_outcome.as_ref(),
                    Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
                )
        }
        SegmentKind::Outro => {
            d.outro_evidence.is_some()
                && matches!(
                    d.outro_outcome.as_ref(),
                    Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
                )
        }
    }
}

fn is_confirmed_absent_in_marker(
    _existing: &StoredMediaMarker,
    detections: &[EpisodeDetection],
    episode: u32,
    kind: SegmentKind,
) -> bool {
    // Check if any detection for this episode was confirmed absent
    detections.iter().any(|d| {
        if d.episode != episode {
            return false;
        }
        match kind {
            SegmentKind::Intro => {
                d.intro_evidence.is_some()
                    && matches!(
                        d.intro_outcome.as_ref(),
                        Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
                    )
            }
            SegmentKind::Outro => {
                d.outro_evidence.is_some()
                    && matches!(
                        d.outro_outcome.as_ref(),
                        Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
                    )
            }
        }
    })
}

/// 章节展示需要一个结束时间；标记本身允许只有片尾起始时间，
/// 因此这里为章节补一个仅供展示的兜底结束时间，不写回标记事实。
fn display_outro_range(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<(i64, i64)> {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if end > start => Some((start, end)),
        (Some(start), None) => Some((start, start + 60_000)),
        _ => None,
    }
}

fn ensure_no_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
) -> Result<(), String> {
    for kind in [SegmentKind::Intro, SegmentKind::Outro] {
        if has_truncated_unmatched(units, detections, collected_evidences, kind) {
            let kind_name = match kind {
                SegmentKind::Intro => "片头",
                SegmentKind::Outro => "片尾",
            };
            return Err(format!(
                "{kind_name}采集音频数据不完整或被截断，未能为全部剧集形成有效标记，保留现有标记"
            ));
        }
    }
    Ok(())
}

/// 某集该 kind 没有标记，但该集同 kind 的采样证据被截断或时长未知时为 true。
fn has_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
    kind: SegmentKind,
) -> bool {
    units.iter().any(|unit| {
        let episode = unit.row.episode.unwrap_or(1);
        let ledger_id = unit.row.id.to_string();
        let detection = find_detection(detections, &ledger_id, episode);
        let has_detection = match kind {
            SegmentKind::Intro => detection.and_then(|d| d.intro_match.as_ref()).is_some(),
            SegmentKind::Outro => detection.and_then(|d| d.outro_match.as_ref()).is_some(),
        };
        if has_detection {
            return false;
        }
        // Episode has no detection for this kind. Check if it had a truncated or unknown PCM evidence.
        collected_evidences.iter().any(|evidence| {
            if evidence.episode != episode || evidence.kind != kind {
                return false;
            }
            match evidence.capture.pcm_duration_ms {
                Some(pcm_ms) => pcm_ms + 1000 < evidence.capture.window.duration_ms(),
                None => true,
            }
        })
    })
}

fn find_detection<'a>(
    detections: &'a [EpisodeDetection],
    ledger_id: &str,
    episode: u32,
) -> Option<&'a EpisodeDetection> {
    detections
        .iter()
        .find(|detection| detection.ledger_id == ledger_id)
        .or_else(|| {
            detections
                .iter()
                .find(|detection| detection.episode == episode)
        })
}

/// 检查所有检测的最终验证状态。
/// 若某个片段已采样并在全季重验证后仍处于未决状态（NeedsFullWindow、SamplingLimit），
/// 说明无法以高置信度产出确定性标记结果，不能标记任务成功并不加区分地重标旧事实。
fn ensure_no_unresolved_forced_verification(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
) -> Result<(), String> {
    let is_forced = units.iter().any(|u| u.force_fingerprint);
    if !is_forced {
        return Ok(());
    }

    // 检查是否存在最终验证未决（如边界截断、由于采样窗口受限无法判定完整边界）
    // 特征：某个采样产生了证据，且其验证结果明确为 NeedsFullWindow 或 SamplingLimit，
    // 且该未决原因并不是因为缺乏模板（"no_valid_templates"）或整季只有单集变体，
    // 而是由于采样窗口到达边界（如 "window_edge_right_boundary_clipped" 或 "ambiguous_consensus_clusters_tie" 等）。
    for d in detections {
        for (kind_name, evidence, outcome) in [
            ("片头", &d.intro_evidence, &d.intro_outcome),
            ("片尾", &d.outro_evidence, &d.outro_outcome),
        ] {
            if evidence.is_some() {
                match outcome {
                    Some(marker::adaptive::VerificationOutcome::NeedsFullWindow { reason }) => {
                        if reason.contains("window_edge")
                            || reason.contains("boundary_clipped")
                            || reason.contains("ambiguous_consensus")
                        {
                            return Err(format!(
                                "剧集第 {} 集{}最终验证未决（边界受限: {reason}），保留现有标记",
                                d.episode, kind_name
                            ));
                        }
                    }
                    Some(marker::adaptive::VerificationOutcome::SamplingLimit { reason }) => {
                        return Err(format!(
                            "剧集第 {} 集{}最终验证未决（采样受限: {reason}），保留现有标记",
                            d.episode, kind_name
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn ensure_no_unresolved_verification(detections: &[EpisodeDetection]) -> Result<(), String> {
    for d in detections {
        for (kind_name, evidence, outcome) in [
            ("片头", &d.intro_evidence, &d.intro_outcome),
            ("片尾", &d.outro_evidence, &d.outro_outcome),
        ] {
            if evidence.is_some() {
                match outcome {
                    Some(marker::adaptive::VerificationOutcome::NeedsFullWindow { reason }) => {
                        return Err(format!(
                            "剧集第 {} 集{}最终验证未决（需要完整窗口: {reason}），保留现有标记",
                            d.episode, kind_name
                        ));
                    }
                    Some(marker::adaptive::VerificationOutcome::SamplingLimit { reason }) => {
                        return Err(format!(
                            "剧集第 {} 集{}最终验证未决（采样受限: {reason}），保留现有标记",
                            d.episode, kind_name
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
