use std::collections::HashSet;
use std::sync::Arc;

use domain::MediaId;
use marker::adaptive::{
    build_season_models, select_seed_episodes, EpisodeDescriptor, EpisodeEvidence,
    SamplingPolicy, SourceCostSummary, TemplateContext,
};
use serde::{Deserialize, Serialize};

use crate::fingerprint_job::{
    capture_episode_adaptive, capture_profile_key, AdaptiveCaptureContext, CapturePolicy,
    EpisodeCaptureRequest, EpisodeDetection, FingerprintCaptureProfile,
};
use crate::scrape_store::ScrapeStoreExt;
use crate::store::MarkerResultReplacement;
use super::markers::prepare_marker_replacement;
use super::progress::MetadataPriorityGate;
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

pub(crate) async fn run_adaptive_season_pipeline(
    mgr: &Arc<ProbeManager>,
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

    let gate = Arc::new(MetadataPriorityGate::new(mgr.clone()));
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

        // If we finished processing all seeds, build template models!
        if is_seed && collected_evidences.len() >= 2 {
            let models = build_season_models(mgr.fingerprint_engine.as_ref(), &collected_evidences, &policy);
            for ev in &collected_evidences {
                templates.references.insert(ev.sample_id.clone(), ev.clone());
            }
            templates.models = models;
        }
    }

    let first_unit = units.first().ok_or("No units to process")?;
    let replacement = prepare_marker_replacement(mgr, first_unit)
        .map_err(|e| format!("Prepare marker replacement error: {e}"))?;

    Ok(replacement)
}
