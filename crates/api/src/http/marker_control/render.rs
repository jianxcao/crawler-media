//! JSON projection helpers for marker rows, per-unit detection outcomes, and job diagnostics.
use axum::response::Response;
use serde_json::{Value, json};

use crate::ScrapeStoreExt;

use super::{known_file_duration_ms, marker_store_error};

pub(super) fn build_tv_episode_rows(
    store: &crate::Store,
    rows: &[domain::FileCoverage],
    season: u32,
) -> Result<Vec<Value>, Response> {
    let mut items = Vec::new();
    for row in rows {
        if row.file.presence != domain::FilePresence::Present
            || row.file.publication != domain::FilePublication::Published
        {
            continue;
        }
        let Some((unit, ep)) = row.units.iter().find_map(|u| {
            u.episode_coordinates()
                .filter(|(s, _)| *s == season)
                .map(|(_, ep)| (*u, ep))
        }) else {
            continue;
        };
        let fid_str = row.file.file_id.to_string();
        let entry = build_file_entry(store, row, unit, Some(season), Some(ep))?;
        items.push((ep, fid_str, entry));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(items.into_iter().map(|(_, _, v)| v).collect())
}

pub(super) fn build_movie_rows(
    store: &crate::Store,
    rows: &[domain::FileCoverage],
) -> Result<Vec<Value>, Response> {
    let mut items = Vec::new();
    for row in rows {
        if row.file.presence != domain::FilePresence::Present
            || row.file.publication != domain::FilePublication::Published
            || !row.units.contains(&domain::UnitKey::WHOLE)
        {
            continue;
        }
        let fid_str = row.file.file_id.to_string();
        let entry = build_file_entry(store, row, domain::UnitKey::WHOLE, None, None)?;
        items.push((fid_str, entry));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(items.into_iter().map(|(_, v)| v).collect())
}

fn build_file_entry(
    store: &crate::Store,
    row: &domain::FileCoverage,
    unit: domain::UnitKey,
    season: Option<u32>,
    episode: Option<u32>,
) -> Result<Value, Response> {
    let fid_str = row.file.file_id.to_string();
    let multi_unit = row.units.len() != 1;
    let duration_ms = known_file_duration_ms(store, row);
    let marker = if multi_unit {
        None
    } else {
        store
            .get_file_marker(row.file.file_id, unit)
            .map_err(marker_store_error)?
            .as_ref()
            .map(marker_json)
    };
    let detection = store
        .latest_probe_unit_for_ledger(&fid_str)
        .map_err(marker_store_error)?
        .map(|u| detection_json(&u, multi_unit));
    Ok(json!({
        "file_id": fid_str,
        "season": season,
        "episode": episode,
        "duration_ms": duration_ms,
        "multi_unit": multi_unit,
        "marker": marker,
        "detection": detection,
    }))
}

pub(super) fn marker_json(m: &store::StoredFileMarker) -> Value {
    json!({
        "intro_start_ms": m.intro_start_ms,
        "intro_end_ms": m.intro_end_ms,
        "outro_start_ms": m.outro_start_ms,
        "outro_end_ms": m.outro_end_ms,
        "source": m.source,
        "locked": m.locked,
        "updated_at": m.updated_at,
    })
}

fn detection_json(unit: &store::ProbeJobUnit, multi_unit: bool) -> Value {
    let parsed = unit
        .detection_outcome_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let kind_status = |key: &str| -> Option<Value> {
        if multi_unit {
            return Some(json!({
                "status": "unsupported",
                "decision": "unsupported",
                "reason": "unit_time_mapping_unsupported",
                "reason_label": "多集单文件暂不支持自动映射",
            }));
        }
        let k = parsed.as_ref()?.get("kinds")?.get(key)?;
        let status = k.get("status").and_then(Value::as_str).unwrap_or("unknown");
        let decision = k
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let reason = k.get("reason").and_then(Value::as_str);
        Some(json!({
            "status": status,
            "decision": decision,
            "reason": reason,
            "reason_label": reason_label_zh(status, decision, reason),
        }))
    };
    json!({
        "job_id": unit.job_id,
        "stage": unit.stage.as_str(),
        "status": unit.status,
        "error": unit.error,
        "intro": kind_status("intro"),
        "outro": kind_status("outro"),
    })
}

fn reason_label_zh(status: &str, decision: &str, reason: Option<&str>) -> &'static str {
    if status == "verified" || decision == "verified" {
        return "声纹共识已验证";
    }
    if status == "no_match" || decision == "no_match" {
        return "无匹配片段";
    }
    let Some(r) = reason else {
        return "待同季更多分集验证";
    };
    if r.contains("insufficient_episodes") || r.contains("insufficient_independent") {
        "有效同季集数不足，无法达成共识"
    } else if r.contains("conflicting") {
        "边界证据存在冲突，保留原标记"
    } else if r.contains("ambiguous") {
        "存在多组相近重复声纹，暂不自动覆盖"
    } else if r.contains("sampling_limit") || r.contains("budget") {
        "采样窗口预算已达上限"
    } else if r.contains("incomplete") || r.contains("unusable") || r.contains("timeline") {
        "音频时间轴或采样不完整"
    } else {
        "声纹候选未决，保留当前标记"
    }
}

pub(crate) fn probe_job_json_enriched(
    store: &crate::Store,
    probe: &crate::probe_manager::ProbeManager,
    job: &store::ProbeJob,
) -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let units = store.probe_job_units(&job.id).unwrap_or_default();
    let phase = if !job.is_active() {
        job.status.as_str()
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Analysis)
    {
        "analyzing"
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Fingerprint && u.status == "running")
    {
        "capturing"
    } else if units
        .iter()
        .any(|u| u.stage == store::ProbeDispatchStage::Metadata && u.status == "running")
    {
        "probing_metadata"
    } else {
        "queued"
    };
    let sampling_mode = units
        .iter()
        .find_map(|u| {
            let plan: Value = serde_json::from_str(u.sampling_plan_json.as_deref()?).ok()?;
            plan.get("sampling_mode")?.as_str().map(String::from)
        })
        .or_else(|| {
            store
                .get_scrape_config()
                .ok()
                .map(|c| c.effective.fingerprint_sampling_mode)
        });
    let timing = probe.timing_snapshot(&job.id);
    let input_bytes = timing.as_ref().and_then(|t| {
        let mut sum = 0u64;
        let mut any = false;
        for att in &t.capture_attempts {
            if let Some(b) = att.metrics.get("input_bytes").and_then(Value::as_u64) {
                sum = sum.saturating_add(b);
                any = true;
            }
        }
        any.then_some(sum)
    });
    json!({
        "id": job.id,
        "kind": job.kind,
        "season": job.season,
        "status": job.status,
        "total": job.total,
        "completed": job.completed,
        "succeeded": job.succeeded,
        "failed": job.failed,
        "error": job.error,
        "elapsed_ms": job.elapsed_ms(now_ms),
        "phase": phase,
        "sampling_mode": sampling_mode,
        "queue_wait_ms": timing.as_ref().and_then(|t| t.stage_totals.queue_wait),
        "priority_wait_ms": timing.as_ref().and_then(|t| t.stage_totals.remote_gate_wait),
        "metrics": {
            "input_bytes": input_bytes,
            "measurement_complete": timing.as_ref().is_some_and(|t| t.persisted_attempts_complete),
        },
    })
}
