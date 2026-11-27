use std::collections::{HashMap, HashSet};

use domain::DownloaderId;
use plugin_api::{BoostSnapshotView, BoostTaskView};
use serde_json::json;
use store::Db;
use store::download_operations::{
    BoostProtectionSets, BoostProtectionState, BoostTaskFactRecord, BoostTaskState,
};
use store::plugins::{PluginCommandTarget, PluginGrantScope, PluginRecord};

use super::config::{BoostConfig, EffectiveBoostRule};
use super::cycle::{BoostCycleSummary, is_live_plugin_active_at, resolve_target_downloader};
use super::snapshot::{
    SnapshotQuery, collect_boost_snapshot_sync, expand_path_set_off_lock,
    expand_protection_sets_off_lock, path_is_in_set,
};
use crate::delivery;
use crate::download_service::plugin_command_cleanup::auto_cleanup_retained_task;
use crate::download_service::plugin_command_ops::{
    collect_db_sibling_paths, execute_delete_task, extend_sibling_paths_from_live_downloaders,
    normalize_task_file_path,
};
use crate::management::ApiState;
use crate::plugins::ExternalEffectStep;

pub(super) fn run_auto_cleanup_pass(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    tasks: &[BoostTaskFactRecord],
    now: i64,
) -> usize {
    let mut cleaned = 0usize;
    for tf in tasks {
        if !is_live_plugin_active_at(state, plugin)
            || resolve_target_downloader(state, cfg) != Ok(dl_id)
        {
            break;
        }
        if tf.downloader_id != dl_id {
            continue;
        }
        if !cfg.sites.is_empty() && !tf.site_id.is_some_and(|s| cfg.sites.contains(&s)) {
            continue;
        }
        let eligible_state = tf.state == BoostTaskState::RetainedOnly
            || (tf.state == BoostTaskState::CleanupPlanned
                && tf.cleanup_plan_expires_at.is_some_and(|e| e < now));
        if !eligible_state {
            continue;
        }
        match auto_cleanup_retained_task(state, plugin, tf, now) {
            Ok(true) => cleaned += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(
                    plugin_id = %plugin.plugin_id,
                    task_fact_id = %tf.task_fact_id,
                    error = %err,
                    "boost auto_cleanup_files task skipped"
                );
            }
        }
    }
    cleaned
}

pub(super) fn run_free_expired_pass(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    snap: &BoostSnapshotView,
    facts_by_id: &HashMap<String, BoostTaskFactRecord>,
    deleted_ids: &mut HashSet<String>,
    summary: &mut BoostCycleSummary,
    now: i64,
) {
    for view in &snap.tasks {
        if deleted_ids.contains(&view.handle) {
            continue;
        }
        if view.state != "downloading" || view.progress_permille >= 1000 {
            continue;
        }
        let Some(tf) = facts_by_id.get(&view.handle) else {
            continue;
        };
        let rule = cfg.effective_for_site(tf.site_id);
        if !rule.delete_on_free_expired {
            continue;
        }
        if !tf.free_until.is_some_and(|fu| fu <= now + 300) {
            continue;
        }
        if delete_task_with_guards(state, plugin, cfg, dl_id, tf, view, false, now).is_some() {
            deleted_ids.insert(tf.task_fact_id.clone());
            summary.evicted_tasks += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_condition_delete_pass(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    snap: &BoostSnapshotView,
    facts_by_id: &HashMap<String, BoostTaskFactRecord>,
    deleted_ids: &mut HashSet<String>,
    summary: &mut BoostCycleSummary,
    now: i64,
) {
    for view in &snap.tasks {
        if deleted_ids.contains(&view.handle) {
            continue;
        }
        if !matches!(view.state.as_str(), "downloading" | "seeding" | "paused") {
            continue;
        }
        let Some(tf) = facts_by_id.get(&view.handle) else {
            continue;
        };
        let rule = cfg.effective_for_site(tf.site_id);
        if !condition_triggered(view, &rule, now) {
            continue;
        }
        if delete_task_with_guards(state, plugin, cfg, dl_id, tf, view, false, now).is_some() {
            deleted_ids.insert(tf.task_fact_id.clone());
            summary.evicted_tasks += 1;
        }
    }
}

fn condition_triggered(view: &BoostTaskView, rule: &EffectiveBoostRule, now: i64) -> bool {
    if let Some(max_seed_h) = rule.max_seed_hours
        && view.state == "seeding"
        && (view.seeding_secs as f64) >= max_seed_h * 3600.0
    {
        return true;
    }
    if let Some(target_ratio) = rule.target_task_ratio {
        let denom = view.downloaded_bytes.max(view.task_bytes);
        if denom > 0 && (view.uploaded_bytes as f64 / denom as f64) >= target_ratio {
            return true;
        }
    }
    if let Some(target_up) = rule.target_uploaded_bytes
        && view.uploaded_bytes >= target_up
    {
        return true;
    }
    if let Some(max_dl_h) = rule.max_download_hours
        && view.state == "downloading"
        && view.added_at.is_some_and(|a| {
            now.saturating_sub(i64::try_from(a).unwrap_or(now)) >= (max_dl_h * 3600.0) as i64
        })
    {
        return true;
    }
    if let Some(min_avg_up) = rule.min_avg_upspeed_bytes
        && let Some(added) = view.added_at
    {
        let elapsed = now.saturating_sub(i64::try_from(added).unwrap_or(now));
        if elapsed >= 60 && (view.uploaded_bytes / u64::try_from(elapsed).unwrap_or(1)) < min_avg_up
        {
            return true;
        }
    }
    if let Some(max_inact_min) = rule.max_inactive_minutes
        && view.upload_speed == 0
        && view.download_speed == 0
        && let Some(last) = view.last_activity_at.or(view.added_at)
    {
        let idle = now.saturating_sub(i64::try_from(last).unwrap_or(now));
        let limit = i64::try_from(max_inact_min)
            .unwrap_or(i64::MAX)
            .saturating_mul(60);
        if idle >= limit {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_dynamic_eviction_pass(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    snap: &mut BoostSnapshotView,
    facts_by_id: &HashMap<String, BoostTaskFactRecord>,
    deleted_ids: &mut HashSet<String>,
    summary: &mut BoostCycleSummary,
    now: i64,
) -> Result<(), String> {
    if !cfg.dynamic_delete {
        return Ok(());
    }
    let Some(budget) = cfg.budget_bytes else {
        return Ok(());
    };
    let budget_u64 = u64::try_from(budget).unwrap_or(0);
    let mut occupied = snap
        .used_bytes
        .saturating_add(snap.reserved_bytes)
        .saturating_add(snap.retained_file_bytes);
    if occupied < budget_u64 {
        return Ok(());
    }
    let watermark = cfg
        .watermark_low_bytes
        .map(|w| u64::try_from(w).unwrap_or(0).min(budget_u64))
        .unwrap_or_else(|| budget_u64.saturating_sub(1));

    let mut candidates: Vec<&BoostTaskView> = snap
        .tasks
        .iter()
        .filter(|t| t.state == "seeding" && !deleted_ids.contains(&t.handle))
        .collect();
    candidates.sort_by_key(|t| {
        (
            t.upload_speed,
            t.added_at.unwrap_or(u64::MAX),
            t.handle.clone(),
        )
    });

    let mut changed = false;
    for view in candidates {
        if occupied <= watermark && occupied < budget_u64 {
            break;
        }
        let Some(tf) = facts_by_id.get(&view.handle) else {
            continue;
        };
        if let Some((evicted, cleaned_bytes)) =
            delete_task_with_guards(state, plugin, cfg, dl_id, tf, view, true, now)
            && evicted
        {
            deleted_ids.insert(tf.task_fact_id.clone());
            summary.evicted_tasks += 1;
            changed = true;
            if let Some(freed) = cleaned_bytes {
                summary.cleaned_files_tasks += 1;
                occupied = occupied.saturating_sub(freed);
            }
        }
    }

    if changed {
        *snap = collect_boost_snapshot_sync(
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
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn delete_task_with_guards(
    state: &ApiState,
    plugin: &PluginRecord,
    cfg: &BoostConfig,
    dl_id: DownloaderId,
    tf: &BoostTaskFactRecord,
    view: &BoostTaskView,
    cleanup_files: bool,
    now: i64,
) -> Option<(bool, Option<u64>)> {
    if !is_live_plugin_active_at(state, plugin)
        || resolve_target_downloader(state, cfg) != Ok(dl_id)
    {
        return None;
    }
    if tf.downloader_id != dl_id || tf.hr_active != Some(false) {
        return None;
    }
    if tf.hr_until.is_some_and(|u| u > now)
        || tf.hold_until.is_some_and(|u| u > now)
        || tf.protection_state != BoostProtectionState::Eligible
        || view.library_referenced
        || view.transfer_active
    {
        return None;
    }
    let Ok(auth) = state.store.issue_plugin_state_authority(plugin, None) else {
        return None;
    };
    let targets = [PluginCommandTarget::Downloader(dl_id)];
    if state
        .store
        .authorize_plugin_command(&auth, "downloader.delete-task", &targets)
        .is_err()
    {
        return None;
    }
    if has_host_protection_conflict(state, dl_id, tf, now) {
        return None;
    }
    let dl_scope = [PluginGrantScope::Object {
        kind: "downloader".into(),
        id: dl_id.to_string(),
    }];
    let Ok(del_raw) = serde_json::to_vec(&json!({
        "task_id": tf.task_fact_id,
        "downloader_id": dl_id.to_string(),
        "expected_revision": tf.revision,
    })) else {
        return None;
    };
    let ExternalEffectStep::Succeeded { .. } =
        execute_delete_task(state, &plugin.plugin_id, &dl_scope, &del_raw, now)
    else {
        return None;
    };
    if !cleanup_files {
        return Some((true, None));
    }
    let Ok(Some(updated_tf)) = state.store.get_boost_task_fact(&tf.task_fact_id) else {
        return Some((true, None));
    };
    match auto_cleanup_retained_task(state, plugin, &updated_tf, now) {
        Ok(true) => Some((
            true,
            Some(u64::try_from(tf.occupied_bytes.max(0)).unwrap_or(0)),
        )),
        Ok(false) => Some((true, None)),
        Err(err) => {
            tracing::warn!(
                plugin_id = %plugin.plugin_id,
                task_fact_id = %tf.task_fact_id,
                error = %err,
                "boost dynamic eviction cleanup failed"
            );
            Some((true, None))
        }
    }
}

fn has_host_protection_conflict(
    state: &ApiState,
    dl_id: DownloaderId,
    tf: &BoostTaskFactRecord,
    now: i64,
) -> bool {
    let Ok((mut protections, all_active, dl_row)) = state
        .store
        .with_read_snapshot(
            &[Db::Library, Db::App, Db::Subscribe],
            std::time::Duration::from_secs(2),
            || {
                let prot = state.store.load_boost_protection_sets()?;
                let all_active = state.store.list_active_boost_task_facts()?;
                let dl_row = state.store.get_downloader(dl_id)?;
                Ok((prot, all_active, dl_row))
            },
        )
        .and_then(|r| r)
    else {
        return true;
    };
    expand_protection_sets_off_lock(&mut protections);

    let exclude_ids = HashSet::from([tf.task_fact_id.clone()]);
    let (mut sib_paths, active_sib_hashes) = collect_db_sibling_paths(&all_active, &exclude_ids);
    let excl_hash = HashSet::from([(dl_id, tf.info_hash.trim().to_ascii_lowercase())]);
    if extend_sibling_paths_from_live_downloaders(
        state,
        &[dl_id],
        &excl_hash,
        &active_sib_hashes,
        &mut sib_paths,
    )
    .is_err()
    {
        return true;
    }
    expand_path_set_off_lock(&mut sib_paths);

    let Ok(client) =
        delivery::frozen_client_for_store_id(&state.store, &state.downloader, Some(dl_id))
    else {
        return true;
    };
    let path_maps = dl_row
        .as_ref()
        .map(|r| r.path_maps.as_slice())
        .unwrap_or(&[]);
    let task_paths = match client.task_files_by_hash(&tf.info_hash) {
        Ok(Some(fset)) => fset
            .files
            .iter()
            .map(|p| normalize_task_file_path(&fset.save_path, p, path_maps))
            .collect::<Vec<_>>(),
        Ok(None) => return true,
        Err(_) => return true,
    };
    let hash_norm = tf.info_hash.trim().to_ascii_lowercase();
    if tf.hr_active != Some(false)
        || tf.hr_until.is_some_and(|u| u > now)
        || tf.hold_until.is_some_and(|u| u > now)
        || protections.active_subscribe_hashes.contains(&hash_norm)
    {
        return true;
    }
    path_list_conflicts(&task_paths, &protections, &sib_paths)
}

fn path_list_conflicts(
    paths: &[String],
    protections: &BoostProtectionSets,
    sibling_paths: &HashSet<String>,
) -> bool {
    paths.iter().any(|p| {
        path_is_in_set(p, &protections.library_paths)
            || path_is_in_set(p, &protections.transfer_paths)
            || path_is_in_set(p, sibling_paths)
    })
}
