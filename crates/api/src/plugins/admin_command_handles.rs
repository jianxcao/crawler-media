use std::str::FromStr;

use domain::{DownloaderId, SiteId, UserId};
use serde_json::Value;
use store::Store;
use store::plugins::{PluginCommandTarget, PluginRecord};

use super::types::PluginServiceError;
use crate::management::ApiState;

fn verify_owned_download_handle(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    handle: &str,
) -> Result<String, PluginServiceError> {
    let verified = state.plugins.verify_ui_handle(
        &plugin.plugin_id,
        plugin.generation,
        actor_user_id,
        Some("owned_download"),
        handle,
    )?;
    Ok(verified.entity_id)
}

fn verify_site_ui_handle(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    handle: &str,
) -> Result<SiteId, PluginServiceError> {
    let verified = state.plugins.verify_ui_handle(
        &plugin.plugin_id,
        plugin.generation,
        actor_user_id,
        Some("site"),
        handle,
    )?;
    SiteId::from_str(&verified.entity_id).map_err(|_| {
        PluginServiceError::invalid("plugin.invalid_handle", "invalid site handle target")
    })
}

fn resolve_indexer_search_handles(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    obj: &mut serde_json::Map<String, Value>,
) -> Result<(), PluginServiceError> {
    if obj.contains_key("sites") && obj.contains_key("site_handle") {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "cannot provide both sites and site_handle",
        ));
    }
    if (obj.contains_key("sites") || obj.contains_key("site_handle"))
        && (obj.contains_key("site_id") || obj.contains_key("site_ids"))
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_handle",
            "cannot combine opaque site handles with raw site_id/site_ids",
        ));
    }
    if let Some(handles_val) = obj.remove("sites") {
        let arr = handles_val.as_array().ok_or_else(|| {
            PluginServiceError::invalid("plugin.invalid_handle", "sites must be an array")
        })?;
        if arr.is_empty() {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "sites must not be empty",
            ));
        }
        let mut ids = Vec::with_capacity(arr.len());
        for item in arr {
            let handle = item.as_str().ok_or_else(|| {
                PluginServiceError::invalid("plugin.invalid_handle", "sites items must be strings")
            })?;
            let sid = verify_site_ui_handle(state, plugin, actor_user_id, handle)?;
            let s = sid.to_string();
            if !ids.iter().any(|v: &Value| v.as_str() == Some(s.as_str())) {
                ids.push(Value::String(s));
            }
        }
        if let Some(first) = ids.first().cloned() {
            obj.insert("site_id".into(), first);
        }
        obj.insert("site_ids".into(), Value::Array(ids));
    } else if let Some(handle_val) = obj.remove("site_handle") {
        let handle = handle_val.as_str().ok_or_else(|| {
            PluginServiceError::invalid("plugin.invalid_handle", "site_handle must be a string")
        })?;
        let sid = verify_site_ui_handle(state, plugin, actor_user_id, handle)?;
        let s = sid.to_string();
        obj.insert("site_id".into(), Value::String(s.clone()));
        obj.insert("site_ids".into(), Value::Array(vec![Value::String(s)]));
    }
    Ok(())
}

fn resolve_prepare_cleanup_handles(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    obj: &mut serde_json::Map<String, Value>,
) -> Result<(), PluginServiceError> {
    let Some(handles_val) = obj.remove("owned_task_handles") else {
        return Ok(());
    };
    if obj.contains_key("task_ids")
        || obj.contains_key("targets")
        || obj.contains_key("task_id")
        || obj.contains_key("downloader_id")
    {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_handle",
            "cannot combine owned_task_handles with raw task_ids/targets/task_id/downloader_id",
        ));
    }
    let arr = handles_val.as_array().ok_or_else(|| {
        PluginServiceError::invalid(
            "plugin.invalid_handle",
            "owned_task_handles must be an array",
        )
    })?;
    let exp_rev = obj.get("expected_revision").and_then(parse_revision_value);
    let mut ids = Vec::with_capacity(arr.len());
    let mut targets = Vec::with_capacity(arr.len());
    for item in arr {
        let handle = item.as_str().ok_or_else(|| {
            PluginServiceError::invalid(
                "plugin.invalid_handle",
                "owned_task_handles items must be strings",
            )
        })?;
        let tid = verify_owned_download_handle(state, plugin, actor_user_id, handle)?;
        ids.push(Value::String(tid.clone()));
        if let Some(rev) = exp_rev {
            targets.push(serde_json::json!({
                "task_id": tid,
                "expected_revision": rev,
            }));
        }
    }
    if !targets.is_empty() {
        obj.insert("targets".into(), Value::Array(targets));
    } else {
        obj.insert("task_ids".into(), Value::Array(ids));
    }
    Ok(())
}

pub(super) fn resolve_ui_command_handles(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    kind: &str,
    payload: &mut Value,
) -> Result<(), PluginServiceError> {
    let Some(obj) = payload.as_object_mut() else {
        return Ok(());
    };
    match kind {
        "site.maintenance" | "site.check-in" | "site.verify-session" => {
            if kind == "site.check-in" && !obj.contains_key("action") {
                obj.insert("action".into(), Value::String("check-in".into()));
            } else if kind == "site.verify-session" && !obj.contains_key("action") {
                obj.insert("action".into(), Value::String("verify-session".into()));
            }
            if let Some(handle_val) = obj.remove("site_handle") {
                let handle = handle_val.as_str().ok_or_else(|| {
                    PluginServiceError::invalid(
                        "plugin.invalid_handle",
                        "site_handle must be a string",
                    )
                })?;
                let sid = verify_site_ui_handle(state, plugin, actor_user_id, handle)?;
                if let Some(existing) = obj.get("site_id").and_then(Value::as_str)
                    && existing != sid.to_string()
                {
                    return Err(PluginServiceError::invalid(
                        "plugin.invalid_handle",
                        "site_handle does not match site_id",
                    ));
                }
                obj.insert("site_id".into(), Value::String(sid.to_string()));
            }
        }
        "indexer.search" => resolve_indexer_search_handles(state, plugin, actor_user_id, obj)?,
        "downloader.delete-task" | "downloader.control" => {
            if let Some(handle_val) = obj.remove("owned_task_handle") {
                if obj.contains_key("task_id")
                    || obj.contains_key("task_fact_id")
                    || obj.contains_key("task_handle")
                    || obj.contains_key("info_hash")
                    || obj.contains_key("hashes")
                    || obj.contains_key("downloader_id")
                {
                    return Err(PluginServiceError::invalid(
                        "plugin.invalid_handle",
                        "cannot combine owned_task_handle with raw task or downloader fields",
                    ));
                }
                let handle = handle_val.as_str().ok_or_else(|| {
                    PluginServiceError::invalid(
                        "plugin.invalid_handle",
                        "owned_task_handle must be a string",
                    )
                })?;
                let tid = verify_owned_download_handle(state, plugin, actor_user_id, handle)?;
                obj.insert("task_id".into(), Value::String(tid));
                if !obj.contains_key("expected_revision")
                    && let Some(rev_val) = obj.remove("revision")
                    && let Some(rev) = parse_revision_value(&rev_val)
                {
                    obj.insert("expected_revision".into(), Value::Number(rev.into()));
                }
            }
        }
        "downloader.prepare-cleanup" => {
            resolve_prepare_cleanup_handles(state, plugin, actor_user_id, obj)?;
        }
        "downloader.delete-files" => {
            if let Some(rev_val) = obj.get("revision")
                && !rev_val.is_null()
                && let Some(rev) = parse_revision_value(rev_val)
            {
                obj.insert("revision".into(), Value::Number(rev.into()));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn normalize_admin_command_kind(store: &Store, plugin_id: &str, kind: &str) -> String {
    if matches!(kind, "site.check-in" | "site.verify-session")
        && let Ok(grants) = store.get_plugin_grants(plugin_id)
    {
        let exact = format!("command:{kind}");
        let has_exact = grants.iter().any(|g| g.capability == exact);
        let has_umbrella = grants
            .iter()
            .any(|g| g.capability == "command:site.maintenance");
        if !has_exact && has_umbrella {
            return "site.maintenance".into();
        }
    }
    kind.to_string()
}

pub(super) fn infer_admin_command_targets(
    store: &Store,
    plugin_id: &str,
    kind: &str,
    payload: &Value,
) -> Vec<PluginCommandTarget> {
    let Some(obj) = payload.as_object() else {
        return vec![PluginCommandTarget::Global];
    };
    match kind {
        "site.maintenance" | "site.check-in" | "site.verify-session" => {
            if let Some(sid) = obj
                .get("site_id")
                .and_then(Value::as_str)
                .and_then(|s| SiteId::from_str(s).ok())
            {
                return vec![PluginCommandTarget::Site(sid)];
            }
        }
        "indexer.search" => {
            if let Some(arr) = obj.get("site_ids").and_then(Value::as_array) {
                let sids: Vec<PluginCommandTarget> = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|s| SiteId::from_str(s).ok())
                    .map(PluginCommandTarget::Site)
                    .collect();
                if !sids.is_empty() {
                    return sids;
                }
            }
            if let Some(sid) = obj
                .get("site_id")
                .and_then(Value::as_str)
                .and_then(|s| SiteId::from_str(s).ok())
            {
                return vec![PluginCommandTarget::Site(sid)];
            }
        }
        "downloader.add" | "downloader.prepare-cleanup" | "downloader.delete-files" => {
            if let Some(dl_id) = obj
                .get("downloader_id")
                .and_then(Value::as_str)
                .and_then(|s| DownloaderId::from_str(s).ok())
            {
                return vec![PluginCommandTarget::Downloader(dl_id)];
            }
            if kind == "downloader.prepare-cleanup"
                && let Some(targets) = infer_prepare_cleanup_targets(store, plugin_id, obj)
            {
                return targets;
            }
            if kind == "downloader.delete-files"
                && let Some(plan_handle) = obj
                    .get("host_file_plan_handle")
                    .or_else(|| obj.get("plan_handle"))
                    .and_then(Value::as_str)
                && let Ok(Some(tf)) = store.get_boost_task_fact_by_plan(plan_handle.trim())
                && tf.plugin_id == plugin_id
            {
                return vec![PluginCommandTarget::Downloader(tf.downloader_id)];
            }
        }
        "downloader.delete-task" | "downloader.control" => {
            if let Some(dl_id) = obj
                .get("downloader_id")
                .and_then(Value::as_str)
                .and_then(|s| DownloaderId::from_str(s).ok())
            {
                return vec![PluginCommandTarget::Downloader(dl_id)];
            }
            let candidates = [
                obj.get("task_fact_id").and_then(Value::as_str),
                obj.get("task_id").and_then(Value::as_str),
                obj.get("task_handle").and_then(Value::as_str),
                obj.get("info_hash").and_then(Value::as_str),
                obj.get("hashes")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_str),
            ];
            for cand in candidates.into_iter().flatten() {
                if let Some(dl_id) = resolve_task_downloader(store, plugin_id, cand) {
                    return vec![PluginCommandTarget::Downloader(dl_id)];
                }
            }
        }
        _ => {}
    }
    vec![PluginCommandTarget::Global]
}

fn resolve_task_downloader(store: &Store, plugin_id: &str, tid: &str) -> Option<DownloaderId> {
    let trimmed = tid.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(Some(tf)) = store.find_boost_task_fact_by_handle(plugin_id, trimmed) {
        return Some(tf.downloader_id);
    }
    store
        .list_owned_download_tasks()
        .ok()?
        .into_iter()
        .find(|t| {
            t.plugin_id.as_deref() == Some(plugin_id)
                && (t.task_id == trimmed
                    || t.ownership_tag == trimmed
                    || t.operation_id == trimmed
                    || t.info_hash
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(trimmed)))
        })
        .map(|t| t.downloader_id)
}

fn infer_prepare_cleanup_targets(
    store: &Store,
    plugin_id: &str,
    obj: &serde_json::Map<String, Value>,
) -> Option<Vec<PluginCommandTarget>> {
    let mut dl_ids = Vec::new();
    let mut push_tid = |tid: &str| {
        if let Some(dl_id) = resolve_task_downloader(store, plugin_id, tid)
            && !dl_ids.contains(&dl_id)
        {
            dl_ids.push(dl_id);
        }
    };
    if let Some(arr) = obj.get("targets").and_then(Value::as_array) {
        for item in arr {
            if let Some(tid) = item.get("task_id").and_then(Value::as_str) {
                push_tid(tid);
            }
        }
    }
    if let Some(arr) = obj.get("task_ids").and_then(Value::as_array) {
        for item in arr {
            if let Some(tid) = item.as_str() {
                push_tid(tid);
            }
        }
    }
    if let Some(tid) = obj.get("task_id").and_then(Value::as_str) {
        push_tid(tid);
    }
    (!dl_ids.is_empty()).then(|| {
        dl_ids
            .into_iter()
            .map(PluginCommandTarget::Downloader)
            .collect()
    })
}

fn parse_revision_value(val: &Value) -> Option<i64> {
    val.as_i64()
        .or_else(|| val.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}
