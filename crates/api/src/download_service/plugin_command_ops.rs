use std::collections::HashSet;
use std::str::FromStr;

use domain::DownloaderId;
use serde::Deserialize;
use serde_json::json;
use store::Db;
use store::download_operations::{
    BoostProtectionSets, BoostProtectionState, BoostTaskFactRecord, BoostTaskState,
    DownloadOwnershipState, UpsertBoostTaskFactInput,
};
use store::plugins::PluginGrantScope;

use super::plugin_command_cleanup::{execute_delete_files, execute_prepare_cleanup};
use crate::delivery;
use crate::management::ApiState;
use crate::plugins::boost::{expand_protection_sets_off_lock, extract_plan_paths, path_is_in_set};
use crate::plugins::command_targets::grant_scopes_cover_downloader;
use crate::plugins::effect::sanitize_prefixed_phase;
use crate::plugins::{ExternalEffectStep, PluginEffectContext};

#[derive(Deserialize)]
pub(super) struct AdminControlPayload {
    #[serde(default)]
    pub(super) task_fact_id: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    #[serde(default)]
    pub(super) expected_revision: Option<i64>,
    pub(super) action: String,
    #[serde(default)]
    pub(super) hashes: Vec<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) upload_limit: Option<u64>,
    #[serde(default)]
    pub(super) download_limit: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct AdminDeleteTaskPayload {
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) task_handle: Option<String>,
    #[serde(default)]
    pub(super) info_hash: Option<String>,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    #[serde(default)]
    pub(super) expected_revision: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct PrepareTargetItem {
    pub(super) task_id: String,
    #[serde(default)]
    pub(super) expected_revision: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct AdminPrepareCleanupPayload {
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    #[serde(default)]
    pub(super) task_ids: Vec<String>,
    #[serde(default)]
    pub(super) targets: Vec<PrepareTargetItem>,
    #[serde(default)]
    pub(super) reason: Option<String>,
    #[serde(default)]
    pub(super) ttl_secs: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct AdminDeleteFilesPayload {
    pub(super) host_file_plan_handle: String,
    #[serde(default)]
    pub(super) revision: Option<i64>,
    #[serde(default)]
    pub(super) confirmation_token: Option<String>,
}

pub(super) fn sanitize_phase(prefix: &str, err: impl std::fmt::Display) -> String {
    sanitize_prefixed_phase(prefix, err)
}

pub(super) fn parse_opt_dl_scope_checked(
    raw: Option<&str>,
    targets: &[PluginGrantScope],
) -> Result<Option<DownloaderId>, ExternalEffectStep> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Ok(dl_id) = DownloaderId::from_str(s) else {
        return Err(ExternalEffectStep::Rejected {
            phase: "invalid-downloader".into(),
        });
    };
    if !grant_scopes_cover_downloader(targets, dl_id) {
        return Err(ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        });
    }
    Ok(Some(dl_id))
}

pub(super) fn collect_prepare_cleanup_items(
    req: &AdminPrepareCleanupPayload,
) -> Result<(String, Option<i64>), &'static str> {
    let mut items: Vec<(String, Option<i64>)> = Vec::new();
    for t in &req.targets {
        let trimmed = t.task_id.trim();
        if trimmed.is_empty() {
            return Err("missing-targets");
        }
        items.push((trimmed.to_string(), t.expected_revision));
    }
    for tid in &req.task_ids {
        let trimmed = tid.trim();
        if trimmed.is_empty() {
            return Err("missing-targets");
        }
        if !items.iter().any(|(id, _)| id == trimmed) {
            items.push((trimmed.to_string(), None));
        } else if req.task_ids.len() > 1 {
            return Err("multiple-targets-unsupported");
        }
    }
    if items.is_empty() {
        return Err("missing-targets");
    }
    if items.len() != 1 {
        return Err("multiple-targets-unsupported");
    }
    Ok(items.remove(0))
}

pub(super) fn find_task_for_plugin(
    store: &store::Store,
    plugin_id: &str,
    handle_opt: Option<&str>,
    dl_opt: Option<DownloaderId>,
) -> Result<Option<BoostTaskFactRecord>, store::StoreError> {
    let Some(raw) = handle_opt.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Some(tf) = store.find_boost_task_fact_by_handle(plugin_id, raw)? {
        if dl_opt.is_none_or(|dl| dl == tf.downloader_id) {
            return Ok(Some(tf));
        }
        return Ok(None);
    }
    if let Some(dl_id) = dl_opt {
        return store.get_boost_task_fact_by_hash(plugin_id, dl_id, raw);
    }
    Ok(None)
}

pub(super) fn verify_owned_task(
    store: &store::Store,
    tf: &BoostTaskFactRecord,
    plugin_id: &str,
) -> Result<(), &'static str> {
    if tf.plugin_id != plugin_id {
        return Err("foreign-task");
    }
    let Ok(Some(own)) = store.get_download_ownership(tf.downloader_id, &tf.ownership_tag) else {
        return Err("ownership-missing");
    };
    if own.plugin_id.as_deref() != Some(plugin_id) {
        return Err("foreign-task");
    }
    if tf.state == BoostTaskState::Cleaned {
        return Err("already-cleaned");
    }
    if matches!(
        tf.state,
        BoostTaskState::Downloading | BoostTaskState::Seeding | BoostTaskState::Paused
    ) && own.ownership_state != DownloadOwnershipState::Owned
    {
        return Err("foreign-or-broken-task");
    }
    Ok(())
}

pub(super) fn check_protection_eligible(
    tf: &BoostTaskFactRecord,
    paths: &[String],
    protections: &BoostProtectionSets,
    sibling_paths: &HashSet<String>,
    now: i64,
    allow_transfer_active: bool,
) -> Result<(), &'static str> {
    if tf.hr_active != Some(false) {
        return Err("protection-denied:hr");
    }
    if tf.hr_until.is_some_and(|u| u > now) {
        return Err("protection-denied:hr-until");
    }
    if tf.hold_until.is_some_and(|u| u > now) {
        return Err("protection-denied:hold-until");
    }
    if tf.protection_state != BoostProtectionState::Eligible {
        return Err("protection-denied:state");
    }
    if !allow_transfer_active {
        let hash_norm = tf.info_hash.trim().to_ascii_lowercase();
        if protections.active_subscribe_hashes.contains(&hash_norm)
            || paths
                .iter()
                .any(|p| path_is_in_set(p, &protections.transfer_paths))
        {
            return Err("protection-denied:transfer-active");
        }
    }
    if paths
        .iter()
        .any(|p| path_is_in_set(p, &protections.library_paths))
    {
        return Err("protection-denied:library-referenced");
    }
    if paths.iter().any(|p| path_is_in_set(p, sibling_paths)) {
        return Err("protection-denied:shared-path");
    }
    Ok(())
}

pub(crate) fn is_active_boost_task_state(state: BoostTaskState) -> bool {
    matches!(
        state,
        BoostTaskState::Downloading | BoostTaskState::Seeding | BoostTaskState::Paused
    )
}

pub(crate) fn collect_db_sibling_paths(
    all_tasks: &[BoostTaskFactRecord],
    exclude_ids: &HashSet<String>,
) -> (HashSet<String>, Vec<(DownloaderId, String)>) {
    let mut set = HashSet::new();
    let mut active_hashes = Vec::new();
    for t in all_tasks {
        if t.state == BoostTaskState::Cleaned || exclude_ids.contains(&t.task_fact_id) {
            continue;
        }
        for p in extract_plan_paths(t) {
            set.insert(p);
        }
        if is_active_boost_task_state(t.state) {
            let h = t.info_hash.trim().to_ascii_lowercase();
            if !h.is_empty() {
                active_hashes.push((t.downloader_id, h));
            }
        }
    }
    (set, active_hashes)
}

pub(crate) fn normalize_task_file_path(
    save_path: &str,
    file: &std::path::Path,
    path_maps: &[downloader::PathMap],
) -> String {
    let base = if file.is_relative() {
        std::path::Path::new(save_path).join(file)
    } else {
        file.to_path_buf()
    };
    downloader::apply_maps(&base, path_maps)
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn extend_sibling_paths_from_live_downloaders(
    state: &ApiState,
    downloader_ids: &[DownloaderId],
    excluded_target_hashes: &HashSet<(DownloaderId, String)>,
    db_active_sibling_hashes: &[(DownloaderId, String)],
    sibling_paths: &mut HashSet<String>,
) -> Result<(), downloader::DownloaderError> {
    for &dl_id in downloader_ids {
        let dl_row = state
            .store
            .get_downloader(dl_id)
            .map_err(|e| downloader::DownloaderError::Message(e.to_string()))?;
        if dl_row.as_ref().is_some_and(|r| !r.enabled) {
            continue;
        }
        let client =
            match delivery::client_for_store_id(&state.store, &state.downloader, Some(dl_id)) {
                Ok(client) => client,
                Err(err) if dl_row.as_ref().is_some_and(|r| r.enabled) => return Err(err),
                Err(_) => continue,
            };
        let is_nil_fallback = dl_id.as_uuid().is_nil() && dl_row.is_none();
        let path_maps = dl_row.map(|r| r.path_maps).unwrap_or_default();
        let mut other_hashes: std::collections::HashMap<String, (String, bool)> =
            std::collections::HashMap::new();
        let snaps = match client.task_snapshots() {
            Ok(snaps) => snaps,
            Err(err)
                if is_nil_fallback && err.to_string().contains("missing default Downloader") =>
            {
                continue;
            }
            Err(err) => return Err(err),
        };
        for snap in snaps {
            let h = snap.info_hash.trim().to_ascii_lowercase();
            if !h.is_empty() && !excluded_target_hashes.contains(&(dl_id, h.clone())) {
                other_hashes.insert(h, (snap.name, true));
            }
        }
        for (sib_dl, sib_h) in db_active_sibling_hashes {
            if *sib_dl == dl_id && !excluded_target_hashes.contains(&(dl_id, sib_h.clone())) {
                other_hashes
                    .entry(sib_h.clone())
                    .or_insert_with(|| (String::new(), false));
            }
        }
        for (hash, (snap_name, from_live_snapshot)) in other_hashes {
            let fset = match client.task_files_by_hash(&hash) {
                Ok(Some(fset)) => fset,
                Ok(None) => continue,
                Err(err) if from_live_snapshot => return Err(err),
                Err(_) => continue,
            };
            if !fset.files.is_empty() {
                for file in &fset.files {
                    sibling_paths.insert(normalize_task_file_path(
                        &fset.save_path,
                        file,
                        &path_maps,
                    ));
                }
            } else if !fset.save_path.trim().is_empty() && !snap_name.trim().is_empty() {
                let joined = std::path::Path::new(&fset.save_path).join(snap_name.trim());
                let mapped = downloader::apply_maps(&joined, &path_maps);
                sibling_paths.insert(mapped.to_string_lossy().into_owned());
            }
        }
    }
    Ok(())
}

pub(super) fn sibling_paths_for(
    store: &store::Store,
    exclude_task_fact_id: &str,
) -> Result<(HashSet<String>, Vec<(DownloaderId, String)>), store::StoreError> {
    let all = store.list_active_boost_task_facts()?;
    let exclude = HashSet::from([exclude_task_fact_id.to_string()]);
    Ok(collect_db_sibling_paths(&all, &exclude))
}

pub(super) fn execute_downloader_command_step(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
) -> ExternalEffectStep {
    let Some(plugin_id) = ctx.plugin_id else {
        return ExternalEffectStep::Rejected {
            phase: "missing-plugin-id".into(),
        };
    };
    let Some(raw) = ctx.payload else {
        return ExternalEffectStep::Rejected {
            phase: "missing-payload".into(),
        };
    };
    match ctx.kind {
        "downloader.control" => execute_control(state, plugin_id, ctx.targets, raw, ctx.now),
        "downloader.delete-task" => {
            execute_delete_task(state, plugin_id, ctx.targets, raw, ctx.now)
        }
        "downloader.prepare-cleanup" => {
            execute_prepare_cleanup(state, plugin_id, ctx.targets, raw, ctx.now)
        }
        "downloader.delete-files" => execute_delete_files(state, ctx, plugin_id, raw),
        _ => ExternalEffectStep::Rejected {
            phase: "unsupported-command".into(),
        },
    }
}

fn resolve_control_task(
    state: &ApiState,
    plugin_id: &str,
    targets: &[PluginGrantScope],
    req: &AdminControlPayload,
) -> Result<BoostTaskFactRecord, ExternalEffectStep> {
    let dl_opt = parse_opt_dl_scope_checked(req.downloader_id.as_deref(), targets)?;
    let handle = req
        .task_fact_id
        .as_deref()
        .or(req.task_id.as_deref())
        .or_else(|| req.hashes.first().map(String::as_str));
    let Ok(Some(tf)) = find_task_for_plugin(&state.store, plugin_id, handle, dl_opt) else {
        return Err(ExternalEffectStep::Rejected {
            phase: "task-not-found".into(),
        });
    };
    if let Err(phase) = verify_owned_task(&state.store, &tf, plugin_id) {
        return Err(ExternalEffectStep::Rejected {
            phase: phase.into(),
        });
    }
    if !grant_scopes_cover_downloader(targets, tf.downloader_id) {
        return Err(ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        });
    }
    if req.expected_revision.is_some_and(|rev| rev != tf.revision) {
        return Err(ExternalEffectStep::Rejected {
            phase: "revision-conflict".into(),
        });
    }
    Ok(tf)
}

fn execute_control(
    state: &ApiState,
    plugin_id: &str,
    targets: &[PluginGrantScope],
    raw: &[u8],
    now: i64,
) -> ExternalEffectStep {
    let Ok(req) = serde_json::from_slice::<AdminControlPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let tf = match resolve_control_task(state, plugin_id, targets, &req) {
        Ok(tf) => tf,
        Err(step) => return step,
    };
    let action = req.action.trim().to_ascii_lowercase();
    if action == "pause"
        && let Err(phase) = check_protection_eligible(
            &tf,
            &[],
            &BoostProtectionSets::default(),
            &HashSet::new(),
            now,
            true,
        )
    {
        return ExternalEffectStep::Rejected {
            phase: phase.into(),
        };
    }
    let Ok(client) = delivery::frozen_client_for_store_id(
        &state.store,
        &state.downloader,
        Some(tf.downloader_id),
    ) else {
        return ExternalEffectStep::Failed {
            phase: "downloader-unavailable".into(),
        };
    };
    let res = match action.as_str() {
        "pause" => client.pause_task(&tf.info_hash),
        "resume" => client.resume_task(&tf.info_hash),
        "tags" => client.set_task_tags(&tf.info_hash, &req.tags),
        "category" => {
            let Some(cat) = req.category.as_deref() else {
                return ExternalEffectStep::Rejected {
                    phase: "missing-category".into(),
                };
            };
            client.set_task_category(&tf.info_hash, cat)
        }
        "limits" => client.set_task_limits(&tf.info_hash, req.upload_limit, req.download_limit),
        _ => {
            return ExternalEffectStep::Rejected {
                phase: "invalid-control-action".into(),
            };
        }
    };
    if let Err(err) = res {
        return ExternalEffectStep::Failed {
            phase: sanitize_phase("downloader-control-failed", err),
        };
    }
    if matches!(action.as_str(), "pause" | "resume") {
        let next_state = if action == "pause" {
            BoostTaskState::Paused
        } else {
            BoostTaskState::Downloading
        };
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
                protection_state: tf.protection_state,
                protection_reason: tf.protection_reason.clone(),
                observed_at: now,
                expected_revision: Some(tf.revision),
                now_utc: now,
            });
    }
    ExternalEffectStep::Succeeded {
        result: serde_json::to_vec(&json!({
            "task_id": tf.task_fact_id,
            "downloader_id": tf.downloader_id.to_string(),
            "info_hash": tf.info_hash,
            "action": action,
        }))
        .ok(),
    }
}

fn load_delete_task_context(
    state: &ApiState,
    plugin_id: &str,
    targets: &[PluginGrantScope],
    raw: &[u8],
) -> Result<
    (
        BoostTaskFactRecord,
        BoostProtectionSets,
        Option<store::DownloaderRow>,
    ),
    ExternalEffectStep,
> {
    let Ok(req) = serde_json::from_slice::<AdminDeleteTaskPayload>(raw) else {
        return Err(ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        });
    };
    let dl_opt = parse_opt_dl_scope_checked(req.downloader_id.as_deref(), targets)?;
    let handle = req
        .task_id
        .as_deref()
        .or(req.task_handle.as_deref())
        .or(req.info_hash.as_deref());
    let Ok(Some(tf)) = find_task_for_plugin(&state.store, plugin_id, handle, dl_opt) else {
        return Err(ExternalEffectStep::Rejected {
            phase: "task-not-found".into(),
        });
    };
    if let Err(phase) = verify_owned_task(&state.store, &tf, plugin_id) {
        return Err(ExternalEffectStep::Rejected {
            phase: phase.into(),
        });
    }
    if !grant_scopes_cover_downloader(targets, tf.downloader_id) {
        return Err(ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        });
    }
    if req.expected_revision.is_some_and(|rev| rev != tf.revision) {
        return Err(ExternalEffectStep::Rejected {
            phase: "revision-conflict".into(),
        });
    }
    if !matches!(
        tf.state,
        BoostTaskState::Downloading | BoostTaskState::Seeding | BoostTaskState::Paused
    ) {
        return Err(ExternalEffectStep::Rejected {
            phase: "invalid-task-state".into(),
        });
    }
    let Ok((mut protections, dl_row)) = state
        .store
        .with_read_snapshot(
            &[Db::Library, Db::App, Db::Subscribe],
            std::time::Duration::from_secs(2),
            || {
                let dl_row = state.store.get_downloader(tf.downloader_id)?;
                let prot = state.store.load_boost_protection_sets()?;
                Ok((prot, dl_row))
            },
        )
        .and_then(|r| r)
    else {
        return Err(ExternalEffectStep::Failed {
            phase: "read-protection-failed".into(),
        });
    };
    expand_protection_sets_off_lock(&mut protections);
    Ok((tf, protections, dl_row))
}

fn resolve_and_verify_delete_task_files(
    client: &dyn downloader::Downloader,
    tf: &BoostTaskFactRecord,
    protections: &BoostProtectionSets,
    path_maps: &[downloader::PathMap],
    now: i64,
) -> Result<(String, Vec<String>), ExternalEffectStep> {
    let fset = match client.task_files_by_hash(&tf.info_hash) {
        Ok(Some(fset)) => fset,
        Ok(None) => {
            return Err(ExternalEffectStep::Rejected {
                phase: "downloader-task-missing".into(),
            });
        }
        Err(err) => {
            return Err(ExternalEffectStep::Failed {
                phase: sanitize_phase("downloader-task-files-failed", err),
            });
        }
    };
    if fset.claimed_by_app {
        return Err(ExternalEffectStep::Rejected {
            phase: "claimed-task-protected".into(),
        });
    }
    let paths: Vec<String> = fset
        .files
        .iter()
        .map(|p| normalize_task_file_path(&fset.save_path, p, path_maps))
        .collect();
    if let Err(phase) =
        check_protection_eligible(tf, &paths, protections, &HashSet::new(), now, false)
    {
        return Err(ExternalEffectStep::Rejected {
            phase: phase.into(),
        });
    }
    let save_path = if fset.save_path.trim().is_empty() {
        fset.save_path
    } else {
        downloader::apply_maps(std::path::Path::new(&fset.save_path), path_maps)
            .to_string_lossy()
            .into_owned()
    };
    Ok((save_path, paths))
}

fn persist_retained_task(
    state: &ApiState,
    tf: &BoostTaskFactRecord,
    paths_json: &str,
    now: i64,
) -> ExternalEffectStep {
    let retained_bytes = tf.occupied_bytes.max(0);
    match state.store.mark_boost_task_retained_with_paths(
        &tf.task_fact_id,
        tf.revision,
        retained_bytes,
        Some(paths_json),
        now,
    ) {
        Ok(updated) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&json!({
                "task_id": updated.task_fact_id,
                "downloader_id": updated.downloader_id.to_string(),
                "info_hash": updated.info_hash,
                "retained_bytes": updated.retained_bytes,
                "revision": updated.revision,
            }))
            .ok(),
        },
        Err(err) => ExternalEffectStep::Failed {
            phase: sanitize_phase("mark-retained-failed", err),
        },
    }
}

pub(crate) fn execute_delete_task(
    state: &ApiState,
    plugin_id: &str,
    targets: &[PluginGrantScope],
    raw: &[u8],
    now: i64,
) -> ExternalEffectStep {
    let (tf, protections, dl_row) = match load_delete_task_context(state, plugin_id, targets, raw) {
        Ok(ctx) => ctx,
        Err(step) => return step,
    };
    if let Err(phase) =
        check_protection_eligible(&tf, &[], &protections, &HashSet::new(), now, false)
    {
        return ExternalEffectStep::Rejected {
            phase: phase.into(),
        };
    }
    let Ok(client) = delivery::frozen_client_for_store_id(
        &state.store,
        &state.downloader,
        Some(tf.downloader_id),
    ) else {
        return ExternalEffectStep::Failed {
            phase: "downloader-unavailable".into(),
        };
    };
    let path_maps = dl_row
        .as_ref()
        .map(|row| row.path_maps.as_slice())
        .unwrap_or(&[]);
    let (save_path, paths) = match resolve_and_verify_delete_task_files(
        client.as_ref(),
        &tf,
        &protections,
        path_maps,
        now,
    ) {
        Ok(v) => v,
        Err(step) => return step,
    };
    let allowed_roots: Vec<String> = dl_row
        .map(|row| {
            row.path_maps
                .into_iter()
                .map(|m| m.to.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    if let Err(err) = client.delete_task(&tf.info_hash, false) {
        return ExternalEffectStep::Failed {
            phase: sanitize_phase("downloader-delete-task-failed", err),
        };
    }
    let paths_json = json!({
        "save_path": save_path,
        "allowed_roots": allowed_roots,
        "paths": paths,
    })
    .to_string();
    persist_retained_task(state, &tf, &paths_json, now)
}
