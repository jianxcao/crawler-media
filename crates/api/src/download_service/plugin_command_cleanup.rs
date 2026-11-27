use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};
use store::Db;
use store::download_operations::{
    BoostTaskFactRecord, BoostTaskState, PrepareBoostCleanupPlanInput,
};
use store::plugins::{
    PluginCommandCause, PluginCommandTarget, PluginGrantScope, PluginRecord, PluginSubject,
};
use uuid::Uuid;

use super::plugin_command_ops::{
    AdminDeleteFilesPayload, AdminPrepareCleanupPayload, check_protection_eligible,
    collect_prepare_cleanup_items, extend_sibling_paths_from_live_downloaders,
    find_task_for_plugin, is_active_boost_task_state, parse_opt_dl_scope_checked, sanitize_phase,
    sibling_paths_for,
};
use crate::management::ApiState;
use crate::plugins::boost::{
    compute_plan_digest, expand_path_set_off_lock, expand_protection_sets_off_lock,
    extract_plan_paths,
};
use crate::plugins::command_targets::grant_scopes_cover_downloader;
use crate::plugins::{ExternalEffectStep, PluginEffectContext};

fn build_cleanup_plan_json(
    tf: &BoostTaskFactRecord,
    dl_row: Option<store::DownloaderRow>,
    paths: &[String],
    reason: String,
) -> String {
    let existing_meta: Value = tf
        .cleanup_plan_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    let save_path = existing_meta
        .get("save_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let allowed_roots: Vec<String> = existing_meta
        .get("allowed_roots")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            dl_row
                .map(|row| {
                    row.path_maps
                        .into_iter()
                        .map(|m| m.to.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        });
    json!({
        "task_fact_id": tf.task_fact_id,
        "downloader_id": tf.downloader_id.to_string(),
        "info_hash": tf.info_hash,
        "save_path": save_path,
        "allowed_roots": allowed_roots,
        "paths": paths,
        "retained_bytes": tf.retained_bytes,
        "reason": reason,
    })
    .to_string()
}

pub(super) fn execute_prepare_cleanup(
    state: &ApiState,
    plugin_id: &str,
    targets: &[PluginGrantScope],
    raw: &[u8],
    now: i64,
) -> ExternalEffectStep {
    let Ok(req) = serde_json::from_slice::<AdminPrepareCleanupPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let dl_opt = match parse_opt_dl_scope_checked(req.downloader_id.as_deref(), targets) {
        Ok(dl) => dl,
        Err(step) => return step,
    };
    let (target_tid, expected_rev_opt) = match collect_prepare_cleanup_items(&req) {
        Ok(item) => item,
        Err(phase) => {
            return ExternalEffectStep::Rejected {
                phase: phase.into(),
            };
        }
    };
    let Ok(Some(tf)) = find_task_for_plugin(&state.store, plugin_id, Some(&target_tid), dl_opt)
    else {
        return ExternalEffectStep::Rejected {
            phase: "task-not-found".into(),
        };
    };
    if !grant_scopes_cover_downloader(targets, tf.downloader_id) {
        return ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        };
    }
    let Ok((protections, sibling_paths, dl_row)) = read_protection_context(state, &tf) else {
        return ExternalEffectStep::Failed {
            phase: "read-protection-failed".into(),
        };
    };

    let paths = extract_plan_paths(&tf);
    if paths.is_empty() || paths.iter().any(|p| p.trim().is_empty()) {
        return ExternalEffectStep::Rejected {
            phase: "manual-cleanup-required".into(),
        };
    }
    if let Err(phase) =
        check_protection_eligible(&tf, &paths, &protections, &sibling_paths, now, false)
    {
        return ExternalEffectStep::Rejected {
            phase: phase.into(),
        };
    }

    let plan_handle = Uuid::new_v4().to_string();
    let ttl_secs = req.ttl_secs.unwrap_or(600).clamp(1, 600);
    let reason = req.reason.unwrap_or_else(|| "boost-cleanup".into());
    let plan_json = build_cleanup_plan_json(&tf, dl_row, &paths, reason);

    match state
        .store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf.task_fact_id.clone(),
            expected_revision: expected_rev_opt.unwrap_or(tf.revision),
            cleanup_plan_handle: plan_handle.clone(),
            cleanup_plan_json: plan_json,
            ttl_secs,
            now_utc: now,
        }) {
        Ok(updated) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&json!({
                "plan_handle": plan_handle,
                "task_id": updated.task_fact_id,
                "revision": updated.revision,
                "expires_at": updated.cleanup_plan_expires_at,
                "paths": paths,
            }))
            .ok(),
        },
        Err(err) => ExternalEffectStep::Rejected {
            phase: sanitize_phase("prepare-cleanup-rejected", err),
        },
    }
}

fn read_protection_context(
    state: &ApiState,
    tf: &BoostTaskFactRecord,
) -> Result<
    (
        store::download_operations::BoostProtectionSets,
        HashSet<String>,
        Option<store::DownloaderRow>,
    ),
    store::StoreError,
> {
    let task_fact_id = tf.task_fact_id.as_str();
    let downloader_id = tf.downloader_id;
    let (mut prot, (mut siblings, db_active_hashes), dl_row, all_dls) = state
        .store
        .with_read_snapshot(
            &[Db::Library, Db::App, Db::Subscribe],
            std::time::Duration::from_secs(2),
            || {
                let prot = state.store.load_boost_protection_sets()?;
                let sib = sibling_paths_for(&state.store, task_fact_id)?;
                let dl_row = state.store.get_downloader(downloader_id)?;
                let all_dls = state.store.list_downloaders()?;
                Ok((prot, sib, dl_row, all_dls))
            },
        )
        .and_then(|r| r)?;
    let mut dl_ids = vec![downloader_id];
    for (sib_dl, _) in &db_active_hashes {
        if !dl_ids.contains(sib_dl) {
            dl_ids.push(*sib_dl);
        }
    }
    for row in &all_dls {
        if row.enabled && !dl_ids.contains(&row.id) {
            dl_ids.push(row.id);
        }
    }
    if !all_dls.iter().any(|r| r.enabled && r.is_default) {
        let nil_id = domain::DownloaderId::from_uuid(uuid::Uuid::nil());
        if !dl_ids.contains(&nil_id) {
            dl_ids.push(nil_id);
        }
    }
    let mut excluded_hashes = HashSet::new();
    if is_active_boost_task_state(tf.state) {
        let h = tf.info_hash.trim().to_ascii_lowercase();
        if !h.is_empty() {
            excluded_hashes.insert((downloader_id, h));
        }
    }
    extend_sibling_paths_from_live_downloaders(
        state,
        &dl_ids,
        &excluded_hashes,
        &db_active_hashes,
        &mut siblings,
    )
    .map_err(|e| store::StoreError::Protected(e.to_string()))?;
    expand_protection_sets_off_lock(&mut prot);
    expand_path_set_off_lock(&mut siblings);
    Ok((prot, siblings, dl_row))
}

fn resolve_host_roots(meta: &Value, dl_row: Option<&store::DownloaderRow>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = dl_row
        .map(|r| r.path_maps.iter().map(|m| m.to.clone()).collect())
        .unwrap_or_default();
    if roots.is_empty()
        && let Some(arr) = meta.get("allowed_roots").and_then(|v| v.as_array())
    {
        for item in arr {
            if let Some(s) = item.as_str().filter(|s| !s.trim().is_empty()) {
                roots.push(PathBuf::from(s));
            }
        }
    }
    if roots.is_empty()
        && let Some(sp) = meta.get("save_path").and_then(|v| v.as_str())
        && !sp.trim().is_empty()
    {
        let p = PathBuf::from(sp);
        if p.is_absolute() && p.parent().is_some() {
            roots.push(p);
        }
    }
    roots
        .into_iter()
        .filter(|r| r.is_dir())
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .filter(|c| c.parent().is_some())
        .collect()
}

fn resolve_missing_parent_via_ancestor(parent: &Path) -> Result<PathBuf, &'static str> {
    let mut cur = parent;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(cur) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(Component::Normal(seg)) = cur.components().next_back() else {
                    return Err("manual-cleanup-required");
                };
                tail.push(seg.to_os_string());
                let Some(next) = cur.parent() else {
                    return Err("manual-cleanup-required");
                };
                cur = next;
            }
            Ok(_) => {
                let Ok(mut canon_anc) = std::fs::canonicalize(cur) else {
                    return Err("manual-cleanup-required");
                };
                if !canon_anc.is_dir() {
                    return Err("manual-cleanup-required");
                }
                for seg in tail.into_iter().rev() {
                    canon_anc.push(seg);
                }
                return Ok(canon_anc);
            }
            Err(_) => return Err("manual-cleanup-required"),
        }
    }
}

fn validate_path_inside_roots(
    path: &Path,
    roots: &[PathBuf],
) -> Result<(PathBuf, PathBuf), &'static str> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path-traversal-rejected");
    }
    let Some(name) = path.file_name() else {
        return Err("invalid-path-parent");
    };
    let Some(parent) = path.parent() else {
        return Err("invalid-path-parent");
    };
    let canon_parent = match std::fs::canonicalize(parent) {
        Ok(cp) => cp,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            resolve_missing_parent_via_ancestor(parent)?
        }
        Err(_) => return Err("manual-cleanup-required"),
    };
    let matching_root = roots
        .iter()
        .find(|root| canon_parent.starts_with(root))
        .ok_or("path-outside-allowed-roots")?;
    Ok((canon_parent.join(name), matching_root.clone()))
}

fn prune_empty_parents(start: &Path, root: &Path) {
    let mut cur = start;
    while cur != root && cur.starts_with(root) {
        if std::fs::remove_dir(cur).is_err() {
            break;
        }
        let Some(parent) = cur.parent() else {
            break;
        };
        cur = parent;
    }
}

fn run_host_fs_deletions(
    paths: &[String],
    roots: &[PathBuf],
) -> Result<Vec<String>, ExternalEffectStep> {
    if paths.is_empty() || paths.iter().any(|p| p.trim().is_empty()) {
        return Err(ExternalEffectStep::Rejected {
            phase: "manual-cleanup-required".into(),
        });
    }
    let mut verified_targets = Vec::with_capacity(paths.len());
    for raw_path in paths {
        let p = PathBuf::from(raw_path);
        match validate_path_inside_roots(&p, roots) {
            Ok((safe_target, canon_root)) => verified_targets.push((safe_target, canon_root)),
            Err(phase) => {
                return Err(ExternalEffectStep::Rejected {
                    phase: phase.into(),
                });
            }
        }
    }

    let mut deleted_paths = Vec::new();
    for (safe_target, canon_root) in &verified_targets {
        match std::fs::symlink_metadata(safe_target) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(ExternalEffectStep::Rejected {
                        phase: "directory-entry-refused".into(),
                    });
                }
                if let Err(err) = std::fs::remove_file(safe_target) {
                    return Err(ExternalEffectStep::Failed {
                        phase: sanitize_phase("host-fs-delete-failed", err),
                    });
                }
                match std::fs::symlink_metadata(safe_target) {
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        deleted_paths.push(safe_target.to_string_lossy().into_owned());
                    }
                    _ => {
                        return Err(ExternalEffectStep::Failed {
                            phase: "host-fs-delete-unverified".into(),
                        });
                    }
                }
                if let Some(parent) = safe_target.parent() {
                    prune_empty_parents(parent, canon_root);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(ExternalEffectStep::Failed {
                    phase: sanitize_phase("host-fs-stat-failed", err),
                });
            }
        }
    }
    Ok(deleted_paths)
}

fn verify_delete_files_token(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
    plugin_id: &str,
    tf: &BoostTaskFactRecord,
    plan_handle: &str,
    expected_rev: i64,
    token_opt: Option<&str>,
) -> Result<(), ExternalEffectStep> {
    let PluginSubject::User(actor_user_id) = ctx.subject else {
        return Err(ExternalEffectStep::Rejected {
            phase: "confirmation-token-actor-required".into(),
        });
    };
    let Ok(Some(plugin)) = state.store.get_plugin(plugin_id) else {
        return Err(ExternalEffectStep::Rejected {
            phase: "plugin-missing".into(),
        });
    };
    let digest = compute_plan_digest(tf.cleanup_plan_json.as_deref().unwrap_or("{}"));
    let tok = token_opt.unwrap_or("");
    if let Err(err) = state.plugins.cleanup_tokens().verify(
        Some(*actor_user_id),
        &plugin.plugin_id,
        &plugin.package_hash,
        plugin.generation,
        plan_handle,
        expected_rev,
        &digest,
        tok,
        true,
    ) {
        return Err(ExternalEffectStep::Rejected {
            phase: sanitize_phase("confirmation-token-rejected", err),
        });
    }
    Ok(())
}

pub(super) fn execute_delete_files(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
    plugin_id: &str,
    raw: &[u8],
) -> ExternalEffectStep {
    let require_confirmation_token =
        !(matches!(ctx.subject, PluginSubject::Task { .. }) && ctx.canonical_method == "GUEST-CMD");
    execute_delete_files_inner(state, ctx, plugin_id, raw, require_confirmation_token)
}

fn plugin_live_for_cleanup(
    store: &store::Store,
    plugin_id: &str,
    expected_gen: Option<i64>,
) -> bool {
    store.get_plugin(plugin_id).ok().flatten().is_some_and(|p| {
        p.lifecycle == store::plugins::PluginLifecycle::Enabled
            && p.desired_enabled
            && expected_gen.is_none_or(|g| g == p.generation)
    })
}

fn execute_delete_files_inner(
    state: &ApiState,
    ctx: &PluginEffectContext<'_>,
    plugin_id: &str,
    raw: &[u8],
    require_confirmation_token: bool,
) -> ExternalEffectStep {
    let Ok(req) = serde_json::from_slice::<AdminDeleteFilesPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let plan_handle = req.host_file_plan_handle.trim();
    let Ok(Some(tf)) = state.store.get_boost_task_fact_by_plan(plan_handle) else {
        return ExternalEffectStep::Rejected {
            phase: "cleanup-plan-missing".into(),
        };
    };
    if tf.plugin_id != plugin_id {
        return ExternalEffectStep::Rejected {
            phase: "foreign-task".into(),
        };
    }
    if !grant_scopes_cover_downloader(ctx.targets, tf.downloader_id) {
        return ExternalEffectStep::Rejected {
            phase: "target-mismatch".into(),
        };
    }
    let expected_rev = req.revision.unwrap_or(tf.revision);
    if tf.state != BoostTaskState::CleanupPlanned
        || tf.revision != expected_rev
        || tf.cleanup_plan_expires_at.is_none_or(|e| e < ctx.now)
    {
        return ExternalEffectStep::Rejected {
            phase: "cleanup-plan-expired-or-conflict".into(),
        };
    }
    if require_confirmation_token
        && let Err(step) = verify_delete_files_token(
            state,
            ctx,
            plugin_id,
            &tf,
            plan_handle,
            expected_rev,
            req.confirmation_token.as_deref(),
        )
    {
        return step;
    }

    let Ok((protections, sibling_paths, dl_row)) = read_protection_context(state, &tf) else {
        return ExternalEffectStep::Failed {
            phase: "read-protection-failed".into(),
        };
    };
    let paths = extract_plan_paths(&tf);
    if paths.is_empty() || paths.iter().any(|p| p.trim().is_empty()) {
        return ExternalEffectStep::Rejected {
            phase: "manual-cleanup-required".into(),
        };
    }
    if let Err(phase) =
        check_protection_eligible(&tf, &paths, &protections, &sibling_paths, ctx.now, false)
    {
        return ExternalEffectStep::Rejected {
            phase: phase.into(),
        };
    }

    let meta: Value = tf
        .cleanup_plan_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    let roots = resolve_host_roots(&meta, dl_row.as_ref());
    if roots.is_empty() {
        return ExternalEffectStep::Rejected {
            phase: "manual-cleanup-required".into(),
        };
    }
    if !plugin_live_for_cleanup(&state.store, plugin_id, ctx.generation) {
        return ExternalEffectStep::Rejected {
            phase: "authority-rejected".into(),
        };
    }
    let deleted_paths = match run_host_fs_deletions(&paths, &roots) {
        Ok(paths) => paths,
        Err(step) => return step,
    };

    match state
        .store
        .complete_boost_cleanup(plan_handle, expected_rev, ctx.now)
    {
        Ok(cleaned) => ExternalEffectStep::Succeeded {
            result: serde_json::to_vec(&json!({
                "kind": "FileOperation",
                "operation_id": ctx.operation_id.to_string(),
                "plan_handle": plan_handle,
                "task_id": cleaned.task_fact_id,
                "downloader_id": cleaned.downloader_id.to_string(),
                "deleted_paths": deleted_paths,
                "freed_bytes": tf.retained_bytes,
                "deleted_at": ctx.now,
                "proof": "host-fs-verified",
            }))
            .ok(),
        },
        Err(err) => ExternalEffectStep::Failed {
            phase: sanitize_phase("complete-cleanup-failed", err),
        },
    }
}

pub(crate) fn auto_cleanup_retained_task(
    state: &ApiState,
    plugin: &PluginRecord,
    tf: &BoostTaskFactRecord,
    now: i64,
) -> Result<bool, String> {
    let auth = state
        .store
        .issue_plugin_state_authority(plugin, None)
        .map_err(|e| e.to_string())?;
    let targets = [PluginCommandTarget::Downloader(tf.downloader_id)];
    state
        .store
        .authorize_plugin_command(&auth, "downloader.prepare-cleanup", &targets)
        .map_err(|e| e.to_string())?;
    state
        .store
        .authorize_plugin_command(&auth, "downloader.delete-files", &targets)
        .map_err(|e| e.to_string())?;

    let dl_scope = [PluginGrantScope::Object {
        kind: "downloader".into(),
        id: tf.downloader_id.to_string(),
    }];
    let prep_raw = serde_json::to_vec(&json!({
        "targets": [{ "task_id": tf.task_fact_id, "expected_revision": tf.revision }],
        "reason": "auto-cleanup",
        "ttl_secs": 600,
    }))
    .map_err(|e| e.to_string())?;
    let (plan_handle, plan_rev) =
        match execute_prepare_cleanup(state, &plugin.plugin_id, &dl_scope, &prep_raw, now) {
            ExternalEffectStep::Succeeded {
                result: Some(bytes),
            } => {
                let val: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let handle = val["plan_handle"]
                    .as_str()
                    .ok_or_else(|| "missing plan_handle".to_string())?
                    .to_string();
                let rev = val["revision"]
                    .as_i64()
                    .ok_or_else(|| "missing plan revision".to_string())?;
                (handle, rev)
            }
            ExternalEffectStep::Rejected { .. } => return Ok(false),
            ExternalEffectStep::Failed { phase }
            | ExternalEffectStep::Unknown { phase }
            | ExternalEffectStep::Deferred { phase, .. } => return Err(phase),
            ExternalEffectStep::Succeeded { result: None } => {
                return Err("missing prepare-cleanup result".into());
            }
        };

    let del_raw = serde_json::to_vec(&json!({
        "host_file_plan_handle": plan_handle,
        "revision": plan_rev,
    }))
    .map_err(|e| e.to_string())?;
    let subject = PluginSubject::Task {
        plugin_id: plugin.plugin_id.clone(),
        task_id: "boost.cycle".into(),
    };
    let cause = PluginCommandCause::root("auto-cleanup");
    let ctx = PluginEffectContext {
        operation_id: domain::PluginOperationId::new(),
        plugin_id: Some(&plugin.plugin_id),
        kind: "downloader.delete-files",
        attempt: 1,
        generation: Some(plugin.generation),
        subject: &subject,
        canonical_method: "GUEST-CMD",
        targets: &dl_scope,
        cause: &cause,
        payload: Some(&del_raw),
        now,
    };
    match execute_delete_files_inner(state, &ctx, &plugin.plugin_id, &del_raw, false) {
        ExternalEffectStep::Succeeded { .. } => Ok(true),
        ExternalEffectStep::Rejected { .. } => Ok(false),
        ExternalEffectStep::Failed { phase }
        | ExternalEffectStep::Unknown { phase }
        | ExternalEffectStep::Deferred { phase, .. } => Err(phase),
    }
}
