use std::collections::{HashMap, HashSet};

use domain::{DownloaderId, Site, SiteId};
use serde_json::json;
use store::download_operations::{
    BoostProtectionState, BoostTaskFactRecord, BoostTaskState, UpsertBoostTaskFactInput,
};
use store::plugins::{
    PluginCommandTarget, PluginGrantScope, PluginLifecycle, PluginRecord, PluginStateKind,
};
use uuid::Uuid;

use super::config::{BoostConfig, quota_key};
use super::cycle_evict::{
    run_auto_cleanup_pass, run_condition_delete_pass, run_dynamic_eviction_pass,
    run_free_expired_pass,
};
use super::cycle_submit::{collect_tracked_hashes, discover_candidates, submit_boost_candidates};
use super::snapshot::{SnapshotQuery, collect_boost_snapshot_sync};
use crate::delivery;
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::plugins::command_targets::{
    grant_scopes_cover_downloader, grant_scopes_cover_site, targets_cover_downloader,
    targets_cover_site,
};
use crate::plugins::effect::sanitize_phase_token;
use crate::plugins::{ExternalEffectStep, PluginEffectContext, PluginServiceError};

#[derive(Debug, Default)]
pub(crate) struct BoostCycleSummary {
    pub dry_run: bool,
    pub added: usize,
    pub skipped_disk: bool,
    pub skipped_quota: bool,
    pub reconciled_tasks: usize,
    pub evicted_tasks: usize,
    pub cleaned_files_tasks: usize,
}

pub(super) fn is_live_plugin_active_at(state: &ApiState, plugin: &PluginRecord) -> bool {
    let Ok(Some(live)) = state.store.get_plugin(&plugin.plugin_id) else {
        return false;
    };
    live.generation == plugin.generation
        && state
            .store
            .issue_plugin_state_authority(&live, None)
            .is_ok()
}

pub(crate) fn validate_admin_boost_run_targets(
    state: &ApiState,
    plugin: &PluginRecord,
    targets: &[PluginCommandTarget],
    payload: &serde_json::Value,
) -> Result<(), PluginServiceError> {
    if targets.contains(&PluginCommandTarget::Global) {
        return Ok(());
    }
    let stored_cfg = state
        .store
        .get_plugin_config(&plugin.plugin_id)?
        .map(|c| c.values)
        .unwrap_or_else(|| json!({}));
    let cfg = BoostConfig::from_layers(&[&stored_cfg, payload])
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e))?;
    if let Some(dl_id) = cfg.downloader_id
        && !targets_cover_downloader(targets, dl_id)
    {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover boost.run downloader_id",
        ));
    }
    if cfg.sites.is_empty()
        || cfg
            .sites
            .iter()
            .any(|sid| !targets_cover_site(targets, *sid))
    {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover boost.run sites",
        ));
    }
    let dl_id = resolve_target_downloader(state, &cfg)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e))?;
    if !targets_cover_downloader(targets, dl_id) {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover boost.run downloader_id",
        ));
    }
    Ok(())
}

pub(crate) fn validate_exec_boost_run_targets(
    state: &ApiState,
    cfg: &BoostConfig,
    targets: &[PluginGrantScope],
) -> bool {
    if targets.contains(&PluginGrantScope::Global) {
        return true;
    }
    let Ok(dl_id) = resolve_target_downloader(state, cfg) else {
        return false;
    };
    grant_scopes_cover_downloader(targets, dl_id)
        && !cfg.sites.is_empty()
        && cfg
            .sites
            .iter()
            .all(|sid| grant_scopes_cover_site(targets, *sid))
}

pub(crate) fn execute_boost_run_command(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
) -> ExternalEffectStep {
    let Some(plugin_id) = ctx.plugin_id else {
        return ExternalEffectStep::Rejected {
            phase: "missing-plugin-id".into(),
        };
    };
    let Ok(Some(plugin)) = state.store.get_plugin(plugin_id) else {
        return ExternalEffectStep::Rejected {
            phase: "plugin-missing".into(),
        };
    };
    if plugin.lifecycle != PluginLifecycle::Enabled
        || !plugin.desired_enabled
        || ctx.generation.is_some_and(|g| g != plugin.generation)
    {
        return ExternalEffectStep::Rejected {
            phase: "authority-rejected".into(),
        };
    }
    let stored_cfg = state
        .store
        .get_plugin_config(plugin_id)
        .ok()
        .flatten()
        .map(|c| c.values)
        .unwrap_or_else(|| json!({}));
    let cmd_val: serde_json::Value = ctx
        .payload
        .and_then(|raw| serde_json::from_slice(raw).ok())
        .unwrap_or_else(|| json!({}));
    let cfg = match BoostConfig::from_layers(&[&stored_cfg, &cmd_val]) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(operation_id = %ctx.operation_id, error = %err, "boost.run config rejected");
            return ExternalEffectStep::Rejected {
                phase: sanitize_phase_token(&err, "invalid-config"),
            };
        }
    };
    if !validate_exec_boost_run_targets(state, &cfg, ctx.targets) {
        return ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        };
    }
    match execute_boost_cycle(state, &plugin, &cfg, &ctx.cause.cause_id) {
        Ok(summary) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&json!({
                "dry_run": summary.dry_run,
                "added": summary.added,
                "skipped_disk": summary.skipped_disk,
                "skipped_quota": summary.skipped_quota,
                "reconciled_tasks": summary.reconciled_tasks,
                "evicted_tasks": summary.evicted_tasks,
                "cleaned_files_tasks": summary.cleaned_files_tasks,
            }))
            .ok(),
        },
        Err(err) => {
            tracing::warn!(operation_id = %ctx.operation_id, error = %err, "boost.run execution failed");
            ExternalEffectStep::Failed {
                phase: sanitize_phase_token(&err, "boost-run-failed"),
            }
        }
    }
}

pub(super) fn resolve_target_downloader(
    state: &ApiState,
    cfg: &BoostConfig,
) -> Result<DownloaderId, String> {
    let all_dls = state.store.list_downloaders().map_err(|e| e.to_string())?;
    if let Some(dl_id) = cfg.downloader_id {
        if let Some(row) = all_dls.iter().find(|r| r.id == dl_id) {
            if !row.enabled {
                return Err("target-downloader-disabled".into());
            }
        } else if !dl_id.as_uuid().is_nil() {
            return Err("target-downloader-missing".into());
        }
        return Ok(dl_id);
    }
    if let Some(row) = state
        .store
        .default_downloader()
        .map_err(|e| e.to_string())?
        && row.enabled
    {
        return Ok(row.id);
    }
    if all_dls.iter().any(|r| r.is_default && !r.enabled) {
        return Err("default-downloader-disabled".into());
    }
    if all_dls.iter().any(|r| r.enabled) {
        return Err("default-downloader-missing".into());
    }
    Ok(DownloaderId::from_uuid(Uuid::nil()))
}

pub(super) struct AuthorizedBoostSite {
    pub(super) site: Site,
    pub(super) can_search: bool,
    pub(super) can_rss: bool,
}

fn authorize_and_resolve_target_sites(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
) -> Result<Vec<AuthorizedBoostSite>, String> {
    let pid = &plugin.plugin_id;
    let auth = state
        .store
        .issue_plugin_state_authority(plugin, None)
        .map_err(|e| e.to_string())?;
    state
        .store
        .authorize_plugin_command(
            &auth,
            "downloader.add",
            &[PluginCommandTarget::Downloader(dl_id)],
        )
        .map_err(|e| format!("{pid} unauthorized for downloader `{dl_id}`: {e}"))?;

    let grants = state
        .store
        .get_plugin_grants(pid)
        .map_err(|e| e.to_string())?;
    let has_cap = |cap: &str, sid: SiteId| -> bool {
        grants.iter().any(|g| {
            g.capability == cap && grant_scopes_cover_site(std::slice::from_ref(&g.scope), sid)
        })
    };
    let site_caps = |sid: SiteId| -> Option<(bool, bool)> {
        if !has_cap("read:sites", sid) {
            return None;
        }
        let can_search = has_cap("command:indexer.search", sid);
        let can_rss = has_cap("command:indexer.rss", sid);
        (can_search || can_rss).then_some((can_search, can_rss))
    };

    if !cfg.sites.is_empty() {
        for sid in &cfg.sites {
            if site_caps(*sid).is_none() {
                return Err(format!("{pid} unauthorized for site `{sid}`"));
            }
        }
    } else if !grants.iter().any(|g| g.capability == "read:sites")
        || !grants.iter().any(|g| {
            g.capability == "command:indexer.search" || g.capability == "command:indexer.rss"
        })
    {
        return Err(format!(
            "{pid} lacks read:sites or command:indexer.search/command:indexer.rss grant"
        ));
    }

    let all_sites = state.store.list_sites().map_err(|e| e.to_string())?;
    let enabled = all_sites.into_iter().filter(|s| s.enabled);
    if cfg.sites.is_empty() {
        return Ok(enabled
            .filter_map(|site| {
                site_caps(site.id).map(|(can_search, can_rss)| AuthorizedBoostSite {
                    site,
                    can_search,
                    can_rss,
                })
            })
            .collect());
    }
    Ok(enabled
        .filter(|s| cfg.sites.contains(&s.id))
        .filter_map(|site| {
            site_caps(site.id).map(|(can_search, can_rss)| AuthorizedBoostSite {
                site,
                can_search,
                can_rss,
            })
        })
        .collect())
}

fn resolve_synced_protection(
    tf: &BoostTaskFactRecord,
    now: i64,
) -> (BoostProtectionState, Option<String>) {
    if tf.protection_state == BoostProtectionState::Protected
        && tf.protection_reason.as_deref() == Some("hold-active")
        && tf.hold_until.is_none_or(|h| h <= now)
    {
        if tf.hr_active == Some(true) || tf.hr_until.is_some_and(|u| u > now) {
            return (BoostProtectionState::Protected, Some("hr-active".into()));
        }
        if tf.hr_active == Some(false) {
            return (BoostProtectionState::Eligible, None);
        }
        return (BoostProtectionState::Unknown, Some("hr-unknown".into()));
    }
    (tf.protection_state, tf.protection_reason.clone())
}

fn sync_live_task_facts(
    state: &ApiState,
    dl_id: DownloaderId,
    tasks: &[BoostTaskFactRecord],
    now: i64,
) -> usize {
    let snaps = delivery::client_for_store_id(&state.store, &state.downloader, Some(dl_id))
        .ok()
        .and_then(|client| client.task_snapshots().ok())
        .unwrap_or_default();
    let mut updated = 0usize;
    for tf in tasks {
        if !matches!(
            tf.state,
            BoostTaskState::Downloading | BoostTaskState::Seeding | BoostTaskState::Paused
        ) {
            continue;
        }
        let next_state = snaps
            .iter()
            .find(|s| s.info_hash.eq_ignore_ascii_case(&tf.info_hash))
            .map_or(tf.state, |live| {
                if live.state.starts_with("paused") || live.state.starts_with("stopped") {
                    BoostTaskState::Paused
                } else if live.progress >= 1.0 {
                    BoostTaskState::Seeding
                } else {
                    BoostTaskState::Downloading
                }
            });
        let (next_prot, next_reason) = resolve_synced_protection(tf, now);
        if next_state != tf.state
            || next_prot != tf.protection_state
            || next_reason != tf.protection_reason
        {
            let _ = state
                .store
                .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
                    task_fact_id: tf.task_fact_id.clone(),
                    plugin_id: tf.plugin_id.clone(),
                    quota_key: tf.quota_key.clone(),
                    downloader_id: tf.downloader_id,
                    ownership_tag: tf.ownership_tag.clone(),
                    info_hash: tf.info_hash.clone(),
                    site_id: tf.site_id,
                    state: next_state,
                    occupied_bytes: tf.occupied_bytes,
                    retained_bytes: tf.retained_bytes,
                    hr_active: tf.hr_active,
                    hr_until: tf.hr_until,
                    hold_until: tf.hold_until,
                    free_until: tf.free_until,
                    protection_state: next_prot,
                    protection_reason: next_reason,
                    observed_at: now,
                    expected_revision: Some(tf.revision),
                    now_utc: now,
                });
            updated += 1;
        }
    }
    updated
}

fn record_kv_summary(
    state: &ApiState,
    plugin: &PluginRecord,
    summary: &BoostCycleSummary,
    now: i64,
) {
    let Ok(auth) = state.store.issue_plugin_state_authority(plugin, None) else {
        return;
    };
    let expected = state
        .store
        .plugin_state_get(&auth, PluginStateKind::Kv, "last_cycle")
        .ok()
        .flatten()
        .map_or(0, |e| e.revision);
    let body = json!({
        "observed_at": now,
        "dry_run": summary.dry_run,
        "added": summary.added,
        "skipped_disk": summary.skipped_disk,
        "skipped_quota": summary.skipped_quota,
        "reconciled_tasks": summary.reconciled_tasks,
        "evicted_tasks": summary.evicted_tasks,
        "cleaned_files_tasks": summary.cleaned_files_tasks,
    });
    if let Ok(bytes) = serde_json::to_vec(&body) {
        let _ = state.store.plugin_state_put(
            &auth,
            PluginStateKind::Kv,
            "last_cycle",
            &bytes,
            expected,
        );
    }
}

fn live_downloader_speeds(
    state: &ApiState,
    dl_id: DownloaderId,
    snap: &plugin_api::BoostSnapshotView,
) -> (u64, u64) {
    if let Ok(client) = delivery::client_for_store_id(&state.store, &state.downloader, Some(dl_id))
        && let Ok(snaps) = client.task_snapshots()
    {
        let up = snaps
            .iter()
            .map(|s| s.upload_speed)
            .fold(0u64, u64::saturating_add);
        let dn = snaps
            .iter()
            .map(|s| s.download_speed)
            .fold(0u64, u64::saturating_add);
        return (up, dn);
    }
    let up = snap
        .tasks
        .iter()
        .map(|t| t.upload_speed)
        .fold(0u64, u64::saturating_add);
    let dn = snap
        .tasks
        .iter()
        .map(|t| t.download_speed)
        .fold(0u64, u64::saturating_add);
    (up, dn)
}

fn evaluate_cycle_admission(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    snap: plugin_api::BoostSnapshotView,
    summary: &mut BoostCycleSummary,
    now: i64,
) -> Result<Option<(plugin_api::BoostSnapshotView, usize)>, String> {
    if !is_live_plugin_active_at(state, plugin) {
        return Ok(None);
    }

    let disk_ok = snap.disk.status == "available"
        && snap
            .disk
            .available_bytes
            .is_some_and(|b| b >= u64::try_from(cfg.min_free_disk_bytes).unwrap_or(0));
    if !disk_ok {
        summary.skipped_disk = true;
        record_kv_summary(state, plugin, summary, now);
        tracing::warn!(
            plugin_id = %plugin.plugin_id,
            disk_status = %snap.disk.status,
            available_bytes = ?snap.disk.available_bytes,
            "boost paused auto-add due to unknown/unavailable/insufficient disk space"
        );
        return Ok(None);
    }

    let downloading_count = snap
        .tasks
        .iter()
        .filter(|t| t.state == "downloading")
        .count();
    if cfg
        .max_downloading
        .is_some_and(|cap| downloading_count >= cap)
    {
        record_kv_summary(state, plugin, summary, now);
        return Ok(None);
    }

    if cfg.global_max_upspeed_bytes.is_some() || cfg.global_max_dlspeed_bytes.is_some() {
        let (total_up, total_dn) = live_downloader_speeds(state, dl_id, &snap);
        if cfg
            .global_max_upspeed_bytes
            .is_some_and(|lim| total_up >= lim)
            || cfg
                .global_max_dlspeed_bytes
                .is_some_and(|lim| total_dn >= lim)
        {
            record_kv_summary(state, plugin, summary, now);
            return Ok(None);
        }
    }

    let active_count = snap
        .tasks
        .iter()
        .filter(|t| matches!(t.state.as_str(), "downloading" | "seeding" | "paused"))
        .count();
    let hourly_used = state
        .store
        .count_boost_adds_since(&plugin.plugin_id, now - 3600)
        .map_err(|e| e.to_string())?;
    let hourly_remaining =
        usize::try_from((cfg.max_adds_per_hour - hourly_used).max(0)).unwrap_or(0);
    let max_rule_round = cfg
        .rules
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|r| r.overrides.max_adds_per_round)
        .fold(cfg.defaults.max_adds_per_round, usize::max);
    let round_cap = max_rule_round
        .min(hourly_remaining)
        .min(cfg.max_active.saturating_sub(active_count));

    if round_cap == 0 {
        record_kv_summary(state, plugin, summary, now);
        return Ok(None);
    }

    if let Some(budget) = cfg.budget_bytes {
        let occupied = snap
            .used_bytes
            .saturating_add(snap.reserved_bytes)
            .saturating_add(snap.retained_file_bytes);
        if occupied >= u64::try_from(budget).unwrap_or(0) {
            summary.skipped_quota = true;
            record_kv_summary(state, plugin, summary, now);
            return Ok(None);
        }
    }

    Ok(Some((snap, round_cap)))
}

fn load_snapshot(
    state: &ApiState,
    plugin: &PluginRecord,
    dl_id: DownloaderId,
) -> Result<plugin_api::BoostSnapshotView, String> {
    collect_boost_snapshot_sync(
        &state.store,
        &state.downloader,
        &SnapshotQuery {
            plugin_id: &plugin.plugin_id,
            site_ids: &[],
            downloader_ids: &[dl_id],
            allowed_sites: None,
            allowed_downloaders: None,
        },
    )
    .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn run_eviction_and_cleanup_passes(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    db_facts: &store::download_operations::BoostSnapshotFacts,
    snap: &mut plugin_api::BoostSnapshotView,
    summary: &mut BoostCycleSummary,
    now: i64,
) -> Result<(), String> {
    let facts_by_id: HashMap<String, BoostTaskFactRecord> = db_facts
        .tasks
        .iter()
        .cloned()
        .map(|tf| (tf.task_fact_id.clone(), tf))
        .collect();
    let mut deleted_ids = HashSet::new();
    run_free_expired_pass(
        state,
        plugin,
        cfg,
        dl_id,
        snap,
        &facts_by_id,
        &mut deleted_ids,
        summary,
        now,
    );
    run_condition_delete_pass(
        state,
        plugin,
        cfg,
        dl_id,
        snap,
        &facts_by_id,
        &mut deleted_ids,
        summary,
        now,
    );
    if cfg.auto_cleanup_files {
        let refreshed = state
            .store
            .get_boost_snapshot_for_plugin(&plugin.plugin_id, &[], &[dl_id])
            .map_err(|e| e.to_string())?;
        summary.cleaned_files_tasks +=
            run_auto_cleanup_pass(state, plugin, cfg, dl_id, &refreshed.tasks, now);
    }
    if summary.evicted_tasks > 0 || summary.cleaned_files_tasks > 0 {
        *snap = load_snapshot(state, plugin, dl_id)?;
    }
    if cfg.dynamic_delete {
        let refreshed = state
            .store
            .get_boost_snapshot_for_plugin(&plugin.plugin_id, &[], &[dl_id])
            .map_err(|e| e.to_string())?;
        let refreshed_by_id: HashMap<String, BoostTaskFactRecord> = refreshed
            .tasks
            .into_iter()
            .map(|tf| (tf.task_fact_id.clone(), tf))
            .collect();
        run_dynamic_eviction_pass(
            state,
            plugin,
            cfg,
            dl_id,
            snap,
            &refreshed_by_id,
            &mut deleted_ids,
            summary,
            now,
        )?;
    }
    Ok(())
}

pub(crate) fn execute_boost_cycle(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    cause_id: &str,
) -> Result<BoostCycleSummary, String> {
    execute_boost_cycle_inner(state, plugin, cfg, cause_id, false)
}

pub(crate) fn execute_boost_cycle_from_task(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    cause_id: &str,
) -> Result<BoostCycleSummary, String> {
    execute_boost_cycle_inner(state, plugin, cfg, cause_id, true)
}

fn execute_boost_cycle_inner(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    cause_id: &str,
    exclude_self_hook: bool,
) -> Result<BoostCycleSummary, String> {
    let now = unix_now();
    let dl_id = resolve_target_downloader(state, cfg)?;
    let sites = authorize_and_resolve_target_sites(state, plugin, cfg, dl_id)?;
    let qkey = quota_key(&plugin.plugin_id, dl_id);

    let mut db_facts = state
        .store
        .get_boost_snapshot_for_plugin(&plugin.plugin_id, &[], &[dl_id])
        .map_err(|e| e.to_string())?;
    let reconciled_tasks = sync_live_task_facts(state, dl_id, &db_facts.tasks, now);
    if reconciled_tasks > 0 {
        db_facts = state
            .store
            .get_boost_snapshot_for_plugin(&plugin.plugin_id, &[], &[dl_id])
            .map_err(|e| e.to_string())?;
    }

    let mut summary = BoostCycleSummary {
        dry_run: cfg.dry_run,
        reconciled_tasks,
        ..Default::default()
    };

    if cfg.dry_run {
        let candidates = discover_candidates(state, cfg, &sites, now);
        record_kv_summary(state, plugin, &summary, now);
        tracing::info!(
            plugin_id = %plugin.plugin_id,
            candidate_count = candidates.len(),
            "boost dry_run cycle completed"
        );
        return Ok(summary);
    }

    let mut snap = load_snapshot(state, plugin, dl_id)?;
    run_eviction_and_cleanup_passes(
        state,
        plugin,
        cfg,
        dl_id,
        &db_facts,
        &mut snap,
        &mut summary,
        now,
    )?;

    let Some((snap, round_cap)) =
        evaluate_cycle_admission(state, plugin, cfg, dl_id, snap, &mut summary, now)?
    else {
        return Ok(summary);
    };

    let candidates = discover_candidates(state, cfg, &sites, now);
    let tracked_enclosures = state
        .store
        .list_active_boost_enclosures(&qkey, dl_id)
        .map_err(|e| e.to_string())?;
    let tracked_hashes = collect_tracked_hashes(&snap);

    submit_boost_candidates(
        state,
        plugin,
        cfg,
        dl_id,
        &qkey,
        cause_id,
        candidates,
        tracked_enclosures,
        &tracked_hashes,
        round_cap,
        exclude_self_hook,
        &mut summary,
    );
    record_kv_summary(state, plugin, &summary, now);
    Ok(summary)
}
