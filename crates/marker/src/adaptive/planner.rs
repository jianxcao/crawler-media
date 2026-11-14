use super::types::{
    EpisodeDescriptor, SamplingPolicy, SegmentKind, SourceCostSummary, TemplateContext,
    WindowDecision,
};
use crate::fingerprint::capture_types::SampleWindow;

pub fn plan_episode_window(
    episode: &EpisodeDescriptor,
    kind: SegmentKind,
    templates: &TemplateContext,
    policy: &SamplingPolicy,
    cost: &SourceCostSummary,
) -> WindowDecision {
    let full_window = default_full_window(episode, kind, policy);

    // If fast path is disabled due to unstable fallbacks (>= 3 fallbacks in last 5 attempts)
    if cost.recent_fast_attempts >= 5 && cost.recent_fallbacks >= 3 {
        return WindowDecision::Full {
            window: full_window,
            reason: "fast_path_disabled_unstable".to_string(),
        };
    }

    // Filter relevant models for this kind (must be stable model)
    let models = prediction_models(episode, kind, templates);

    if models.is_empty() {
        return WindowDecision::Full {
            window: full_window,
            reason: "no_stable_template_models".to_string(),
        };
    }

    let late_prediction = kind == SegmentKind::Intro
        && models.iter().any(|model| {
            model
                .references
                .iter()
                .any(|reference| reference.match_interval_ms.1 > full_window.end_ms)
        });
    // Calculate predicted envelopes for all models
    let mut predicted_ranges = Vec::new();
    let mut used_model_ids = Vec::new();

    for model in &models {
        if let Some((start_pred, end_pred)) = predict_interval_for_episode(model, episode) {
            // Apply context margins
            let window_start = (start_pred - policy.context_margin_ms).max(0);
            let window_end =
                (end_pred + policy.context_margin_ms).min(prediction_bound(episode, kind));

            if window_end > window_start {
                predicted_ranges.push((window_start, window_end));
                used_model_ids.push(model.model_id.clone());
            }
        }
    }

    if predicted_ranges.is_empty() {
        return WindowDecision::Full {
            window: full_window,
            reason: "prediction_failed".to_string(),
        };
    }

    // Merge all candidate envelopes into one contiguous window
    let mut min_start = predicted_ranges[0].0;
    let mut max_end = predicted_ranges[0].1;
    for &(s, e) in &predicted_ranges[1..] {
        min_start = min_start.min(s);
        max_end = max_end.max(e);
    }

    // Clamp within default search range
    let clamped_start = min_start.max(full_window.start_ms);
    let clamped_end = if late_prediction {
        max_end
    } else {
        max_end.min(full_window.end_ms)
    };

    let verify_window = SampleWindow {
        start_ms: clamped_start,
        end_ms: clamped_end,
    };

    if let Err(e) = verify_window.validate() {
        return WindowDecision::Full {
            window: full_window,
            reason: format!("invalid_predicted_window: {e}"),
        };
    }

    if verify_window.duration_ms() > 300_000 {
        return WindowDecision::Full {
            window: full_window,
            reason: "prediction_exceeds_capture_bound".into(),
        };
    }
    // The initial window cannot discover a late prediction, so its cost is irrelevant.
    if late_prediction && verify_window.end_ms > full_window.end_ms {
        return WindowDecision::Verify {
            window: verify_window,
            model_ids: used_model_ids,
        };
    }
    if let Some(reason) = cost_fallback(&full_window, &verify_window, policy, cost) {
        return WindowDecision::Full {
            window: full_window,
            reason: reason.into(),
        };
    }

    WindowDecision::Verify {
        window: verify_window,
        model_ids: used_model_ids,
    }
}

fn prediction_models<'a>(
    episode: &EpisodeDescriptor,
    kind: SegmentKind,
    templates: &'a TemplateContext,
) -> Vec<&'a super::types::TemplateModel> {
    let mut models = templates
        .models
        .iter()
        .filter(|model| model.kind == kind && model.is_stable && !model.references.is_empty())
        .collect::<Vec<_>>();
    // A validated current target reference is more precise than other episodes'
    // positions. Keep all matching models so conflicting predictions stay bounded.
    if kind == SegmentKind::Intro
        && models.iter().any(|model| {
            model
                .references
                .iter()
                .any(|reference| target_reference(reference, episode))
        })
    {
        models.retain(|model| {
            model
                .references
                .iter()
                .any(|reference| target_reference(reference, episode))
        });
    }
    models
}

fn target_reference(
    reference: &super::types::TemplateReference,
    episode: &EpisodeDescriptor,
) -> bool {
    reference.ledger_id == episode.ledger_id && reference.source_version == episode.source_version
}

fn prediction_bound(episode: &EpisodeDescriptor, kind: SegmentKind) -> i64 {
    if kind == SegmentKind::Intro {
        if episode.duration_ms > 0 {
            episode.duration_ms.min(600_000)
        } else {
            600_000
        }
    } else {
        episode.duration_ms
    }
}

fn cost_fallback(
    full_window: &SampleWindow,
    verify_window: &SampleWindow,
    policy: &SamplingPolicy,
    cost: &SourceCostSummary,
) -> Option<&'static str> {
    let full_duration_ms = full_window.duration_ms();
    let verify_duration_ms = verify_window.duration_ms();

    // Check cost improvement or 15% window saving threshold
    let savings_ratio = (full_duration_ms - verify_duration_ms) as f64 / full_duration_ms as f64;

    if cost.measured_samples >= 3 {
        // Use H / R / P formula
        let h = cost.median_first_pcm_ms.unwrap_or(300) as f64;
        let r = cost.sample_ms_per_wall_ms.unwrap_or(10.0);
        let p = if cost.recent_fast_attempts > 0 {
            cost.recent_fallbacks as f64 / cost.recent_fast_attempts as f64
        } else {
            0.0
        };

        let c_full = h + (full_duration_ms as f64 / r);
        let c_adaptive = h + (verify_duration_ms as f64 / r) + (p * c_full);

        if c_adaptive > 0.90 * c_full {
            return Some("cost_saving_below_threshold");
        }
    } else if savings_ratio < policy.min_window_saving_ratio {
        return Some("window_saving_below_threshold");
    }

    None
}

pub fn default_full_window(
    episode: &EpisodeDescriptor,
    kind: SegmentKind,
    policy: &SamplingPolicy,
) -> SampleWindow {
    let window_ms = (policy.full_window_duration_secs as i64) * 1000;
    match kind {
        SegmentKind::Intro => SampleWindow {
            start_ms: 0,
            end_ms: if episode.duration_ms > 0 {
                episode.duration_ms.min(window_ms)
            } else {
                window_ms
            },
        },
        SegmentKind::Outro => {
            let dur = episode.duration_ms;
            if dur > 0 && dur < window_ms {
                SampleWindow {
                    start_ms: 0,
                    end_ms: dur,
                }
            } else {
                let actual_dur = dur.max(window_ms);
                SampleWindow {
                    start_ms: (actual_dur - window_ms).max(0),
                    end_ms: actual_dur,
                }
            }
        }
    }
}

fn predict_interval_for_episode(
    model: &super::types::TemplateModel,
    episode: &EpisodeDescriptor,
) -> Option<(i64, i64)> {
    match model.kind {
        SegmentKind::Intro => {
            // Use the current file's own interval when available; a new file still
            // needs the conservative envelope across independent references.
            let has_own = model
                .references
                .iter()
                .any(|r| target_reference(r, episode));
            let mut starts = Vec::new();
            let mut ends = Vec::new();
            for r in model
                .references
                .iter()
                .filter(|r| !has_own || target_reference(r, episode))
            {
                starts.push(r.match_interval_ms.0);
                ends.push(r.match_interval_ms.1);
            }
            let min_start = *starts.iter().min()?;
            let max_end = *ends.iter().max()?;
            Some((min_start, max_end))
        }
        SegmentKind::Outro => {
            // Predict based on offset from media end
            let mut offsets_start = Vec::new();
            let mut offsets_end = Vec::new();

            for r in &model.references {
                if let Some((d1, d2)) = r.match_from_end_ms {
                    let d_start = d1.max(d2);
                    let d_end = d1.min(d2);
                    offsets_start.push(d_start);
                    offsets_end.push(d_end);
                } else {
                    // Approximate if not stored directly
                    let length = r.match_interval_ms.1 - r.match_interval_ms.0;
                    offsets_start.push(length);
                    offsets_end.push(0);
                }
            }

            let max_from_end_start = *offsets_start.iter().max()?;
            let min_from_end_end = *offsets_end.iter().min()?;

            let predicted_start = episode.duration_ms - max_from_end_start;
            let predicted_end = episode.duration_ms - min_from_end_end;
            Some((predicted_start, predicted_end))
        }
    }
}
