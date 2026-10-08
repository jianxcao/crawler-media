use std::collections::HashSet;
use std::sync::Arc;

use domain::MediaId;
use marker::adaptive::{
    build_season_models, select_seed_episodes, EpisodeDescriptor, EpisodeEvidence,
    SamplingPolicy, SourceCostSummary, TemplateContext,
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

pub async fn run_adaptive_season_pipeline(
    mgr: &ProbeManager,
    job_id: &str,
    media_id: MediaId,
    season: u32,
    units: &[ProbeUnit],
) -> Result<MarkerResultReplacement, String> {
    let sampling_mode = {
        let store = mgr.store.lock();
        store
            .get_scrape_config()
            .ok()
            .map(|c| c.effective.fingerprint_sampling_mode)
            .unwrap_or_else(|| "full_window".to_string())
    };

    let profile = FingerprintCaptureProfile::default();
    let profile_key = capture_profile_key(&profile);
    let policy = SamplingPolicy::default();

    // Collect descriptors
    let mut descriptors = Vec::new();
    for u in units {
        let path = std::path::Path::new(&u.row.path);
        let src_ver = crate::fingerprint_job::current_source_version(path);
        descriptors.push(EpisodeDescriptor {
            ledger_id: u.row.id.to_string(),
            episode: u.row.episode.unwrap_or(1),
            source_version: src_ver,
            duration_ms: 0, // will be updated from store cache
        });
    }

    // Update duration_ms from store
    {
        let store = mgr.store.lock();
        for d in &mut descriptors {
            if let Ok(Some(cached)) = store.get_media_info_cache_version(&d.ledger_id) {
                if let Some(dur) = cached.format_duration_ms {
                    d.duration_ms = dur;
                }
            }
        }
    }

    let seed_ids = select_seed_episodes(&descriptors, &policy);
    let seed_set: HashSet<String> = seed_ids.iter().cloned().collect();

    let _plan = SeasonSamplingPlan {
        job_id: job_id.to_string(),
        media_id: media_id.to_string(),
        season,
        sampling_mode: sampling_mode.clone(),
        analysis_policy_key: "default".to_string(),
        seed_ledger_ids: seed_ids.clone(),
    };

    let gate = mgr.priority_gate();
    let ctx = AdaptiveCaptureContext {
        store: mgr.store.clone(),
        matcher: mgr.fingerprint_engine.clone(),
        capture: mgr.capture_engine.clone(),
        gate,
    };

    // 1. Process seed units first
    let mut collected_evidences: Vec<EpisodeEvidence> = Vec::new();
    let mut detections: Vec<EpisodeDetection> = Vec::new();
    let cost_summary = SourceCostSummary::default();

    // Order units: seeds first, then non-seeds
    let mut ordered_units = Vec::new();
    for u in units {
        if seed_set.contains(&u.row.id.to_string()) {
            ordered_units.push(u);
        }
    }
    for u in units {
        if !seed_set.contains(&u.row.id.to_string()) {
            ordered_units.push(u);
        }
    }

    let mut templates = TemplateContext::default();

    for u in ordered_units {
        let is_seed = seed_set.contains(&u.row.id.to_string());
        let path = std::path::Path::new(&u.row.path);
        let src_ver = crate::fingerprint_job::current_source_version(path);
        let duration_ms = {
            let store = mgr.store.lock();
            store
                .get_media_info_cache_version(&u.row.id.to_string())
                .ok()
                .flatten()
                .and_then(|v| v.format_duration_ms)
        };

        let capture_policy = if u.reuse_fingerprint_cache {
            CapturePolicy::ReuseValid
        } else {
            CapturePolicy::Recapture
        };

        let req = EpisodeCaptureRequest {
            job_id: job_id.to_string(),
            row: u.row.clone(),
            source_version: src_ver,
            media_duration_ms: duration_ms,
            audio_stream_index: None,
            capture_profile_key: profile_key.clone(),
            policy: policy.clone(),
            capture_policy,
            templates: templates.clone(),
            cost_summary: cost_summary.clone(),
        };

        let detection = capture_episode_adaptive(&ctx, &req).await?;

        // Update cost summary if fallback occurred
        if let Some(ref ev) = detection.intro_evidence {
            collected_evidences.push(ev.clone());
        }
        if let Some(ref ev) = detection.outro_evidence {
            collected_evidences.push(ev.clone());
        }

        detections.push(detection);

        // If we finished processing all seeds, build template models and persist them!
        if is_seed && collected_evidences.len() >= 2 {
            let models = build_season_models(mgr.fingerprint_engine.as_ref(), &collected_evidences, &policy);
            for model in &models {
                let model_json = serde_json::to_string(model).unwrap_or_else(|_| "{}".to_string());
                let stored_model = StoredFingerprintModel {
                    model_id: model.model_id.clone(),
                    media_id: media_id.to_string(),
                    season,
                    kind: match model.kind {
                        SegmentKind::Intro => "intro".to_string(),
                        SegmentKind::Outro => "outro".to_string(),
                    },
                    model_version: 1,
                    membership_key: format!("{}:{}", media_id, season),
                    policy_key: "default".to_string(),
                    model_json,
                    created_at_ms: std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                };
                let members: Vec<StoredFingerprintModelMember> = model.references.iter().map(|r| {
                    StoredFingerprintModelMember {
                        model_id: model.model_id.clone(),
                        sample_id: r.sample_id.clone(),
                        ledger_id: r.ledger_id.clone(),
                        source_version: r.source_version.clone(),
                    }
                }).collect();

                let store = mgr.store.lock();
                if let Err(e) = store.put_fingerprint_model(&stored_model, &members) {
                    tracing::warn!(error = %e, "failed to persist fingerprint season model");
                }
            }

            for ev in &collected_evidences {
                templates.references.insert(ev.sample_id.clone(), ev.clone());
            }
            templates.models = models;

            // 模板形成后，立即重新分析之前的建模集，回填匹配区间，避免建模集的标记丢失
            for det in detections.iter_mut() {
                if det.intro_match.is_none() {
                    if let Some(ref ev) = det.intro_evidence {
                        if let marker::adaptive::VerificationOutcome::Verified(v) =
                            marker::adaptive::verify_template_window(
                                mgr.fingerprint_engine.as_ref(),
                                ev,
                                &templates,
                                &policy,
                            )
                        {
                            det.intro_match = Some(v);
                        }
                    }
                }
                if det.outro_match.is_none() {
                    if let Some(ref ev) = det.outro_evidence {
                        if let marker::adaptive::VerificationOutcome::Verified(v) =
                            marker::adaptive::verify_template_window(
                                mgr.fingerprint_engine.as_ref(),
                                ev,
                                &templates,
                                &policy,
                            )
                        {
                            det.outro_match = Some(v);
                        }
                    }
                }
            }
        }
    }

    // 在最终构建标记前，对所有仍未匹配的单元进行回填
    for det in detections.iter_mut() {
        if det.intro_match.is_none() {
            if let Some(matching_ref) = templates
                .models
                .iter()
                .filter(|m| m.kind == SegmentKind::Intro)
                .find_map(|m| m.references.iter().find(|r| r.episode == det.episode))
            {
                det.intro_match = Some(marker::adaptive::VerifiedInterval {
                    start_ms: matching_ref.match_interval_ms.0,
                    end_ms: matching_ref.match_interval_ms.1,
                    supporting_episodes: 2,
                    score: 0.0,
                    coverage: 1.0,
                });
            } else if let Some(ref ev) = det.intro_evidence {
                if let marker::adaptive::VerificationOutcome::Verified(v) =
                    marker::adaptive::verify_template_window(
                        mgr.fingerprint_engine.as_ref(),
                        ev,
                        &templates,
                        &policy,
                    )
                {
                    det.intro_match = Some(v);
                }
            }
        }
        if det.outro_match.is_none() {
            if let Some(matching_ref) = templates
                .models
                .iter()
                .filter(|m| m.kind == SegmentKind::Outro)
                .find_map(|m| m.references.iter().find(|r| r.episode == det.episode))
            {
                det.outro_match = Some(marker::adaptive::VerifiedInterval {
                    start_ms: matching_ref.match_interval_ms.0,
                    end_ms: matching_ref.match_interval_ms.1,
                    supporting_episodes: 2,
                    score: 0.0,
                    coverage: 1.0,
                });
            } else if let Some(ref ev) = det.outro_evidence {
                if let marker::adaptive::VerificationOutcome::Verified(v) =
                    marker::adaptive::verify_template_window(
                        mgr.fingerprint_engine.as_ref(),
                        ev,
                        &templates,
                        &policy,
                    )
                {
                    det.outro_match = Some(v);
                }
            }
        }
    }

    let mut markers = Vec::new();
    let mut chapter_updates = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    {
        let store = mgr.store.lock();
        for u in units {
            let ep = u.row.episode.unwrap_or(1);
            let detection = detections.iter().find(|d| d.ledger_id == u.row.id.to_string() || d.episode == ep);
            let intro_range = detection.and_then(|d| d.intro_match.as_ref()).map(|v| (v.start_ms, v.end_ms));
            let outro_range = detection.and_then(|d| d.outro_match.as_ref()).map(|v| (v.start_ms, v.end_ms));

            if intro_range.is_some() || outro_range.is_some() {
                markers.push(StoredMediaMarker {
                    media_id,
                    season,
                    episode: ep,
                    intro_start_ms: intro_range.map(|r| r.0),
                    intro_end_ms: intro_range.map(|r| r.1),
                    outro_start_ms: outro_range.map(|r| r.0),
                    outro_end_ms: outro_range.map(|r| r.1),
                    source: "fingerprint_adaptive".to_string(),
                    locked: false,
                    updated_at: now,
                });
            }

            let existing = store
                .get_cached_chapters(&u.row.id.to_string())
                .ok()
                .flatten()
                .unwrap_or_default();
            let complete = marker::build_complete_timeline_chapters(&existing, intro_range, outro_range, None);
            chapter_updates.push((u.row.id.to_string(), complete));
        }
    }

    let replacement = MarkerResultReplacement {
        media_id,
        season,
        markers,
        chapter_updates,
    };

    Ok(replacement)
}
