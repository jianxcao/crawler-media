use std::str::FromStr;

use domain::{DownloaderId, UserId};
use serde_json::Value;
use store::download_operations::BoostTaskState;
use store::plugins::{PluginCommandTarget, PluginRecord};

use super::plugin_command_ops::{
    AdminControlPayload, AdminDeleteFilesPayload, AdminDeleteTaskPayload,
    AdminPrepareCleanupPayload, collect_prepare_cleanup_items, find_task_for_plugin,
    verify_owned_task,
};
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::plugins::PluginServiceError;
use crate::plugins::boost::compute_plan_digest;
use crate::plugins::command_targets::targets_cover_downloader;

fn parse_opt_dl_checked(
    raw: Option<&str>,
    targets: &[PluginCommandTarget],
) -> Result<Option<DownloaderId>, PluginServiceError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let dl_id = DownloaderId::from_str(s).map_err(|_| {
        PluginServiceError::invalid("plugin.invalid_input", "invalid downloader_id")
    })?;
    if !targets_cover_downloader(targets, dl_id) {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover payload downloader_id",
        ));
    }
    Ok(Some(dl_id))
}

fn ensure_dl_target(
    targets: &[PluginCommandTarget],
    dl_id: DownloaderId,
) -> Result<(), PluginServiceError> {
    if !targets_cover_downloader(targets, dl_id) {
        return Err(PluginServiceError::forbidden(
            "plugin.forbidden",
            "command targets do not cover task downloader_id",
        ));
    }
    Ok(())
}

fn validate_admin_control(
    state: &ApiState,
    plugin: &PluginRecord,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let parsed: AdminControlPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    let dl_opt = parse_opt_dl_checked(parsed.downloader_id.as_deref(), targets)?;
    let handle = parsed
        .task_fact_id
        .as_deref()
        .or(parsed.task_id.as_deref())
        .or_else(|| parsed.hashes.first().map(String::as_str));
    let tf = find_task_for_plugin(&state.store, &plugin.plugin_id, handle, dl_opt)?.ok_or_else(
        || {
            PluginServiceError::forbidden(
                "plugin.foreign_task",
                "target task is not owned by this plugin",
            )
        },
    )?;
    verify_owned_task(&state.store, &tf, &plugin.plugin_id)
        .map_err(|msg| PluginServiceError::forbidden("plugin.foreign_task", msg))?;
    ensure_dl_target(targets, tf.downloader_id)
}

fn validate_admin_delete_task(
    state: &ApiState,
    plugin: &PluginRecord,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let parsed: AdminDeleteTaskPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    let dl_opt = parse_opt_dl_checked(parsed.downloader_id.as_deref(), targets)?;
    let handle = parsed
        .task_id
        .as_deref()
        .or(parsed.task_handle.as_deref())
        .or(parsed.info_hash.as_deref());
    let tf = find_task_for_plugin(&state.store, &plugin.plugin_id, handle, dl_opt)?.ok_or_else(
        || {
            PluginServiceError::forbidden(
                "plugin.foreign_task",
                "target task is not owned by this plugin",
            )
        },
    )?;
    verify_owned_task(&state.store, &tf, &plugin.plugin_id)
        .map_err(|msg| PluginServiceError::forbidden("plugin.foreign_task", msg))?;
    ensure_dl_target(targets, tf.downloader_id)
}

fn validate_admin_prepare_cleanup(
    state: &ApiState,
    plugin: &PluginRecord,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let parsed: AdminPrepareCleanupPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    let dl_opt = parse_opt_dl_checked(parsed.downloader_id.as_deref(), targets)?;
    let (target_tid, _) = collect_prepare_cleanup_items(&parsed)
        .map_err(|msg| PluginServiceError::invalid("plugin.invalid_input", msg))?;
    let tf = find_task_for_plugin(&state.store, &plugin.plugin_id, Some(&target_tid), dl_opt)?
        .ok_or_else(|| {
            PluginServiceError::forbidden(
                "plugin.foreign_task",
                "target task is not owned by this plugin",
            )
        })?;
    ensure_dl_target(targets, tf.downloader_id)
}

fn validate_admin_delete_files(
    state: &ApiState,
    actor_user_id: UserId,
    plugin: &PluginRecord,
    targets: &[PluginCommandTarget],
    payload: &Value,
    now: i64,
) -> Result<(), PluginServiceError> {
    if payload.get("from_plugin").is_some() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "from_plugin is not allowed in admin command payloads",
        ));
    }
    let parsed: AdminDeleteFilesPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    let token = parsed.confirmation_token.as_deref().unwrap_or("");
    if token.trim().is_empty() {
        return Err(PluginServiceError::forbidden(
            "plugin.cleanup_token_required",
            "confirmation_token is required",
        ));
    }
    let tf = state
        .store
        .get_boost_task_fact_by_plan(parsed.host_file_plan_handle.trim())?
        .filter(|tf| tf.plugin_id == plugin.plugin_id)
        .ok_or_else(|| {
            PluginServiceError::not_found("plugin.cleanup_plan_missing", "cleanup plan not found")
        })?;
    ensure_dl_target(targets, tf.downloader_id)?;
    if tf.state != BoostTaskState::CleanupPlanned
        || tf.cleanup_plan_expires_at.is_none_or(|exp| exp < now)
    {
        return Err(PluginServiceError::conflict(
            "plugin.cleanup_plan_expired",
            "cleanup plan is expired or inactive",
        ));
    }
    let expected_rev = parsed.revision.unwrap_or(tf.revision);
    if expected_rev != tf.revision {
        return Err(PluginServiceError::conflict(
            "plugin.revision_conflict",
            "cleanup plan revision mismatch",
        ));
    }
    let digest = compute_plan_digest(tf.cleanup_plan_json.as_deref().unwrap_or("{}"));
    state.plugins.cleanup_tokens().verify(
        Some(actor_user_id),
        &plugin.plugin_id,
        &plugin.package_hash,
        plugin.generation,
        parsed.host_file_plan_handle.trim(),
        expected_rev,
        &digest,
        token,
        false,
    )
}

pub(crate) fn validate_admin_command(
    state: &ApiState,
    actor_user_id: UserId,
    plugin: &PluginRecord,
    kind: &str,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let now = unix_now();
    match kind {
        "downloader.control" => validate_admin_control(state, plugin, targets, payload),
        "downloader.delete-task" => validate_admin_delete_task(state, plugin, targets, payload),
        "downloader.prepare-cleanup" => {
            validate_admin_prepare_cleanup(state, plugin, targets, payload)
        }
        "downloader.delete-files" => {
            validate_admin_delete_files(state, actor_user_id, plugin, targets, payload, now)
        }
        _ => Ok(()),
    }
}
