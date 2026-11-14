use std::collections::HashSet;

use domain::MediaId;
use marker::SegmentKind;
use marker::adaptive::{
    EpisodeDescriptor, EpisodeEvidence, SamplingPolicy, SourceCostSummary, TemplateContext,
    TemplateModel, build_season_models, select_seed_episodes,
};
use serde::{Deserialize, Serialize};

use super::ProbeUnit;
use super::single_episode;

#[path = "season_access.rs"]
mod access;
use crate::fingerprint_job::{
    AdaptiveCaptureContext, CapturePolicy, EpisodeCaptureRequest, EpisodeDetection,
    capture_episode_adaptive, capture_profile_key,
};
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{MarkerResultReplacement, StoredFileMarker};
use access::SeasonAccess;
pub use access::run_adaptive_season_pipeline;

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
    mgr: &'a SeasonAccess,
    job_id: &'a str,
    media_id: MediaId,
    season: u32,
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

#[cfg(test)]
mod tests;

async fn run_adaptive_season_pipeline_inner(
    mgr: &SeasonAccess,
    job_id: &str,
    media_id: MediaId,
    season: u32,
    units: &[ProbeUnit],
) -> Result<MarkerResultReplacement, String> {
    for unit in units {
        super::require_single_unit(&unit.row).map_err(|error| error.to_string())?;
    }
    let sampling_mode = effective_sampling_mode(mgr)?;
    let policy = SamplingPolicy::default();

    // Collect descriptors
    let descriptors = collect_episode_descriptors(mgr, units)?;
    let seed_ids = select_seed_episodes(&descriptors, &policy);
    let seed_set: HashSet<String> = seed_ids.iter().cloned().collect();

    let _plan = SeasonSamplingPlan {
        job_id: job_id.to_string(),
        media_id: media_id.to_string(),
        season,
        sampling_mode,
        analysis_policy_key: crate::fingerprint_job::analysis_policy_key(&policy),
        seed_ledger_ids: seed_ids,
    };

    let (gate, _leases) = mgr.gate.clone().lease_rows(units)?;
    let ctx = adaptive_capture_context(mgr, std::sync::Arc::new(gate), job_id);
    let pipeline = SeasonPipeline {
        mgr,
        job_id,
        media_id,
        season,
        policy: &policy,
        ctx: &ctx,
        cost_summary: SourceCostSummary::default(),
    };

    // 1. 种子单元优先处理，边采集边建模；2. 其余单元用已有模板验证。
    let mut state = SeasonPipelineState::default();
    for unit in order_seed_units_first(units, &seed_set) {
        let availability = pipeline.mgr.store_access().source_available(&unit.row);
        if let Err(error) = availability {
            if !crate::fingerprint_job::source::is_file_deleted(&error) {
                return Err(error);
            }
            pipeline.mgr.cancel_unit(unit, "file_deleted");
            return Err(error);
        }
        let is_seed = seed_set.contains(&unit.row.file.file_id.to_string());
        match run_season_unit(&pipeline, unit, is_seed, &mut state).await {
            Ok(()) => {}
            Err(e) if e.starts_with("file_deleted") => {
                pipeline.mgr.cancel_unit(unit, "file_deleted");
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }

    ensure_season_sources(&pipeline, units)?;

    // 在所有单元采集完成后，使用全季收集的全部有效证据重新建模，
    // 发现可能存在的第二种变体或在种子阶段未识别出的模板，并重新验证所有单元。
    if state.collected_evidences.len() >= 2 {
        build_and_persist_templates(&pipeline, &state.collected_evidences, &mut state.templates)?;
    }
    reverify_detections(&pipeline, &state.templates, &mut state.detections);

    let (markers, chapter_updates) = build_marker_replacement(&pipeline, units, &state.detections)?;

    // 检查是否存在由于音频截断或覆盖不全被拒绝的证据：
    // 按 kind 独立检查，不能因为一个 kind（如 Intro）有标记就忽略另一个 kind（如 Outro）的音频截断失败。
    // 如果某个 kind 未能产出标记或模型，但该 kind 的采样证据中存在截断，必须报错退出，避免旧标记被清空。
    ensure_no_truncated_unmatched(units, &state.detections, &state.collected_evidences)?;

    // A manual refresh may publish only confirmed matches or confirmed absence.
    ensure_no_unresolved_verification(&state.detections)?;

    ensure_season_sources(&pipeline, units)?;
    Ok(MarkerResultReplacement {
        media_id,
        season,
        targets: units.iter().map(|unit| unit.row.clone()).collect(),
        markers,
        chapter_updates,
    })
}

fn ensure_season_sources(pipeline: &SeasonPipeline<'_>, units: &[ProbeUnit]) -> Result<(), String> {
    for unit in units {
        let active = pipeline
            .mgr
            .store
            .fingerprint_source_active(pipeline.job_id, &unit.row.file.file_id.to_string())
            .map_err(|error| format!("source_check_failed: {error}"))?;
        if !active {
            return Err("file_deleted: probe source cancelled".into());
        }
        if pipeline.ctx.gate.work_control(&unit.row).is_cancelled() {
            return Err("Library probe source cancelled or changed".into());
        }
        let availability = pipeline.mgr.store_access().source_available(&unit.row);
        if let Err(error) = availability {
            if crate::fingerprint_job::source::is_file_deleted(&error) {
                pipeline.mgr.cancel_unit(unit, "file_deleted");
            }
            return Err(error);
        }
    }
    Ok(())
}

fn effective_sampling_mode(mgr: &SeasonAccess) -> Result<String, String> {
    let store = mgr.store.as_ref();
    Ok(store
        .get_scrape_config()
        .map_err(|error| error.to_string())?
        .effective
        .fingerprint_sampling_mode)
}

fn collect_episode_descriptors(
    mgr: &SeasonAccess,
    units: &[ProbeUnit],
) -> Result<Vec<EpisodeDescriptor>, String> {
    let observed = units
        .iter()
        .map(|unit| {
            (
                unit,
                crate::fingerprint_job::current_source_version(&unit.row.file.path),
            )
        })
        .collect::<Vec<_>>();
    let store = mgr.store.as_ref();
    Ok(observed
        .into_iter()
        .filter_map(|(unit, source_version)| {
            let ledger_id = unit.row.file.file_id.to_string();
            Some(EpisodeDescriptor {
                ledger_id: ledger_id.clone(),
                episode: single_episode(&unit.row)?,
                source_version,
                duration_ms: known_media_duration(&store, &ledger_id).unwrap_or(0),
            })
        })
        .collect())
}

fn known_media_duration(store: &crate::Store, ledger_id: &str) -> Option<i64> {
    store
        .get_media_info_cache_version(ledger_id)
        .ok()
        .flatten()
        .and_then(|cached| cached.format_duration_ms)
}

fn adaptive_capture_context(
    mgr: &SeasonAccess,
    gate: std::sync::Arc<dyn crate::fingerprint_job::CaptureGate>,
    job_id: &str,
) -> AdaptiveCaptureContext {
    AdaptiveCaptureContext {
        store: mgr.store_access(),
        matcher: mgr.fingerprint_engine.clone(),
        capture: std::sync::Arc::new(
            crate::fingerprint_job::capture_measurement::CaptureMeasurement {
                engine: mgr.capture_engine.clone(),
                timings: mgr.timings.clone(),
                job_id: Some(job_id.into()),
            },
        ),
        gate,
        timings: Some(mgr.timings.clone()),
    }
}

fn order_seed_units_first<'a>(
    units: &'a [ProbeUnit],
    seed_set: &HashSet<String>,
) -> Vec<&'a ProbeUnit> {
    let is_seed = |unit: &ProbeUnit| seed_set.contains(&unit.row.file.file_id.to_string());
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
    let ledger_id = unit.row.file.file_id.to_string();
    let path = std::path::Path::new(&unit.row.file.path);
    let capture_policy = if unit.reuse_fingerprint_cache {
        CapturePolicy::ReuseValid
    } else {
        CapturePolicy::Recapture
    };

    let media_duration_ms = {
        let reader = pipeline.mgr.store.as_ref();
        known_media_duration(&reader, &ledger_id)
    };
    let selected = pipeline
        .mgr
        .audio_selections
        .lock()
        .get(&unit.row.file.path)
        .copied();
    let source = crate::fingerprint_job::source_for_file_selected(
        &pipeline.mgr.store,
        &unit.row.file,
        selected,
    );
    let profile = crate::fingerprint_job::profile_for_source(source.as_ref());
    let req = EpisodeCaptureRequest {
        job_id: pipeline.job_id.to_string(),
        row: unit.row.clone(),
        source_version: crate::fingerprint_job::current_source_version(path),
        media_duration_ms,
        audio_stream_index: source.as_ref().map(|source| source.audio_stream_index),
        capture_profile_key: capture_profile_key(&profile),
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
        build_and_persist_templates(pipeline, &state.collected_evidences, &mut state.templates)?;
        reverify_detections(pipeline, &state.templates, &mut state.detections);
    }
    Ok(())
}

fn build_and_persist_templates(
    pipeline: &SeasonPipeline<'_>,
    collected_evidences: &[EpisodeEvidence],
    templates: &mut TemplateContext,
) -> Result<(), String> {
    let comparison_start = std::time::Instant::now();
    let (mut models, observation) = crate::fingerprint_job::measurement::EvidenceMeasurement::new(
        pipeline.mgr.fingerprint_engine.clone(),
        pipeline.policy,
    )
    .measure(|engine| build_season_models(engine, collected_evidences, pipeline.policy));
    pipeline
        .mgr
        .timings
        .record_analysis(Some(pipeline.job_id), &observation);
    pipeline.mgr.timings.record_comparison(
        Some(pipeline.job_id),
        comparison_start.elapsed().as_millis() as u64,
    );

    let mut bundles = Vec::new();
    for model in &mut models {
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
        bundles.push(prepare_season_model(pipeline, model)?);
    }

    persist_season_models(pipeline, &bundles)?;
    for evidence in collected_evidences {
        templates
            .references
            .insert(evidence.sample_id.clone(), evidence.clone());
    }
    templates.models = models;
    Ok(())
}

fn prepare_season_model(
    pipeline: &SeasonPipeline<'_>,
    model: &mut TemplateModel,
) -> Result<store::StoredFingerprintModelBundle, String> {
    let samples = model
        .references
        .iter()
        .map(|reference| {
            pipeline
                .mgr
                .store
                .fingerprint_sample_by_id(&reference.sample_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "model reference sample missing".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = store::StoredFingerprintModelBundle::new(
        pipeline.media_id,
        pipeline.season,
        crate::fingerprint_job::analysis_policy_key(pipeline.policy),
        model.clone(),
        samples,
        now_ms(),
    )
    .map_err(|error| error.to_string())?;
    *model = serde_json::from_str(&bundle.model.model_json).map_err(|error| error.to_string())?;
    let bundle = pipeline
        .mgr
        .store
        .bind_fingerprint_model(bundle)
        .map_err(|error| error.to_string())?;

    Ok(bundle)
}

fn persist_season_models(
    pipeline: &SeasonPipeline<'_>,
    bundles: &[store::StoredFingerprintModelBundle],
) -> Result<(), String> {
    let model_set = store::StoredFingerprintModelSet::new(
        pipeline.media_id,
        pipeline.season,
        crate::fingerprint_job::analysis_policy_key(pipeline.policy),
        bundles.to_vec(),
    )
    .map_err(|error| error.to_string())?;
    pipeline.mgr.store.library_write(|store| {
    // Comparison runs without Store. Recheck the durable origin under the same
    // writer guard as model publication; never reacquire Registry from here.
    let origin = match store.get_probe_job(pipeline.job_id) {
        Ok(origin) => origin,
        Err(error) => {
            tracing::error!(%error, job_id=pipeline.job_id, "season model origin lookup failed");
            return Err(error);
        }
    };
    if origin.as_ref().is_some_and(|job| !job.is_active()) {
        tracing::info!(
            job_id = pipeline.job_id,
            "cancelled season model publication"
        );
        return Err(store::StoreError::Missing("file_deleted: season model source cancelled".into()));
    }
    match store.probe_job_units(pipeline.job_id) {
        Ok(units)
            if units
                .iter()
                .any(|unit| matches!(unit.status.as_str(), "cancelled" | "failed")) =>
        {
            tracing::info!(
                job_id = pipeline.job_id,
                "cancelled season model member publication"
            );
            return Err(store::StoreError::Missing("file_deleted: season model source cancelled".into()));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, job_id=pipeline.job_id, "season model member lookup failed");
            return Err(error);
        }
    }
    let automatic = origin
        .as_ref()
        .is_some_and(|job| job.kind == "fingerprint_probe");
    if bundles.iter().flat_map(|bundle| &bundle.members).any(|member| {
        super::ledger_id(&member.ledger_id)
            .and_then(|file_id| store.file_coverage_by_id(file_id).ok().flatten())
            .is_none_or(|row| {
                row.file.media_id != pipeline.media_id
                    || row.file.version.source_version != member.source_version
                    || crate::fingerprint_job::source::ensure_source_recorded_on(store, &row).is_err()
                    || (automatic
                        && !store
                            .get_library(&row.file.library_id.to_string())
                            .ok()
                            .flatten()
                            .is_some_and(|library| {
                                library.detect_intros && library.enable_fingerprint
                            }))
            })
    }) {
        tracing::warn!(
            job_id = pipeline.job_id,
            media_id = %pipeline.media_id,
            "cancelled Library season model publication"
        );
        return Err(store::StoreError::Missing("file_deleted: season model source cancelled".into()));
    }
    if let Err(error) = store.replace_fingerprint_model_bundles(&model_set) {
        if matches!(
            error,
            store::StoreError::Protected(_) | store::StoreError::Missing(_)
        ) {
            tracing::warn!(%error, media_id=%pipeline.media_id,
                "cancelled stale fingerprint season model publication");
        } else {
            tracing::error!(%error, media_id=%pipeline.media_id,
                "failed to persist fingerprint season model");
        }
        return Err(error);
    }
    Ok(())
    }).map_err(|error| error.to_string())
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

    let chapters = match pipeline.mgr.store.get_cached_chapters(&evidence.ledger_id) {
        Ok(chapters) => chapters
            .unwrap_or_default()
            .into_iter()
            .filter(|chapter| !chapter.synthetic)
            .map(|chapter| marker::boundary::Interval {
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            *matched = None;
            *outcome_dest = Some(marker::VerificationOutcome::NeedsFullWindow {
                reason: format!("boundary_chapter_read_failed: {error}"),
            });
            return;
        }
    };
    let comparison_start = std::time::Instant::now();
    let (outcome, observation) = crate::fingerprint_job::measurement::EvidenceMeasurement::new(
        pipeline.mgr.fingerprint_engine.clone(),
        pipeline.policy,
    )
    .measure(|engine| {
        marker::adaptive::verify_template_window_with_chapters(
            engine,
            evidence,
            templates,
            pipeline.policy,
            &chapters,
        )
    });
    pipeline
        .mgr
        .timings
        .record_analysis(Some(pipeline.job_id), &observation);
    match outcome {
        marker::adaptive::VerificationOutcome::Verified(ref verified) => {
            *matched = Some(verified.clone());
        }
        _ => {
            *matched = None;
        }
    }
    *outcome_dest = Some(outcome);
    pipeline.mgr.timings.record_comparison(
        Some(pipeline.job_id),
        comparison_start.elapsed().as_millis() as u64,
    );
}

#[path = "season_versions.rs"]
mod versions;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[path = "season_publication.rs"]
mod publication;
use publication::{
    build_marker_replacement, ensure_no_truncated_unmatched, ensure_no_unresolved_verification,
};
