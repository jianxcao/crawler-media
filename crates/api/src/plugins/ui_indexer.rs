use std::str::FromStr;

use domain::{Site, SiteId, UserId};
use serde::Deserialize;
use serde_json::{Value, json};
use store::plugins::{PluginOperationState, PluginRecord};
use uuid::Uuid;

use super::effect::{ExternalEffectStep, PluginEffectContext};
use super::service::run_app_read;
use super::types::PluginServiceError;
use crate::management::ApiState;

pub(crate) async fn query_indexer_cached(
    state: &ApiState,
    plugin: &PluginRecord,
    actor_user_id: UserId,
    op_id: Uuid,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let pid = plugin.plugin_id.clone();
    let admin = run_app_read(state, move |store| {
        let Some(admin) = store.get_plugin_operation_admin(op_id)? else {
            return Err(PluginServiceError::not_found(
                "plugin.operation_missing",
                "search operation not found",
            ));
        };
        if admin.operation.plugin_id.as_deref() != Some(pid.as_str())
            || admin.operation.kind != "indexer.search"
        {
            return Err(PluginServiceError::not_found(
                "plugin.operation_missing",
                "search operation does not belong to this plugin",
            ));
        }
        if store
            .get_plugin_operation(&store::plugins::PluginSubject::User(actor_user_id), op_id)?
            .is_none()
        {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "search operation belongs to a different subject",
            ));
        }
        Ok(admin)
    })
    .await?;

    if matches!(
        admin.operation.state,
        PluginOperationState::Queued | PluginOperationState::Running
    ) {
        return Err(PluginServiceError::conflict(
            "plugin.operation_not_ready",
            "search operation is still queued or running",
        ));
    }

    let fallback_site_id = admin
        .result_json
        .as_ref()
        .and_then(|v| v.get("site_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let raw_items = admin
        .result_json
        .and_then(|v| v.get("items").and_then(Value::as_array).cloned())
        .unwrap_or_default();

    let uid = actor_user_id.to_string();
    let mut sanitized = Vec::with_capacity(raw_items.len().min(limit));
    for (idx, item) in raw_items.into_iter().take(limit).enumerate() {
        let entity_id = format!("{op_id}:{idx}");
        let (handle, _) = state.plugins.ui_handles().mint(
            &plugin.plugin_id,
            plugin.generation,
            &uid,
            "torrent",
            &entity_id,
            1,
        )?;
        sanitized.push(sanitize_cached_torrent(&item, &fallback_site_id, handle));
    }

    Ok(json!({
        "search_operation_id": op_id,
        "state": admin.operation.state,
        "items": sanitized,
        "next_cursor": Value::Null,
    }))
}

fn sanitize_cached_torrent(item: &Value, fallback_site_id: &str, handle: String) -> Value {
    let site_id = item
        .get("site_id")
        .and_then(Value::as_str)
        .unwrap_or(fallback_site_id);
    let title = item.get("title").and_then(Value::as_str).unwrap_or("");
    let size = item
        .get("size")
        .or_else(|| item.get("size_bytes"))
        .and_then(Value::as_u64);
    let seeders = item.get("seeders").and_then(Value::as_u64);
    let leechers = item.get("leechers").and_then(Value::as_u64);
    let published_at = item
        .get("published_at")
        .or_else(|| item.get("upload_time"))
        .and_then(Value::as_str);
    let free = item.get("free").and_then(Value::as_bool);
    let hr = item.get("hr").and_then(Value::as_bool);
    let category = item.get("category").and_then(Value::as_str);
    let mut labels: Vec<String> = item
        .get("labels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if labels.is_empty() {
        if free == Some(true) {
            labels.push("free".into());
        }
        if hr == Some(true) {
            labels.push("hr".into());
        }
        if let Some(cat) = category.filter(|c| !c.trim().is_empty()) {
            labels.push(cat.to_string());
        }
    }
    json!({
        "handle": handle,
        "site_id": site_id,
        "title": title,
        "size": size,
        "size_bytes": size,
        "seeders": seeders,
        "leechers": leechers,
        "published_at": published_at,
        "free": free,
        "hr": hr,
        "category": category,
        "labels": labels,
    })
}

#[derive(Deserialize)]
struct IndexerSearchPayload {
    #[serde(default)]
    site_ids: Option<Vec<SiteId>>,
    #[serde(default)]
    site_handle: Option<String>,
    #[serde(default)]
    sites: Option<Vec<String>>,
    #[serde(default)]
    site_id: Option<SiteId>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    page: Option<u32>,
}

impl IndexerSearchPayload {
    fn search_term(&self) -> Option<&str> {
        self.keyword
            .as_deref()
            .or(self.query.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

fn resolve_payload_site_ids(
    state: &ApiState,
    plugin_id: &str,
    generation: i64,
    actor_user_id: UserId,
    parsed: &IndexerSearchPayload,
) -> Result<Vec<SiteId>, PluginServiceError> {
    if let Some(ids) = parsed.site_ids.as_ref().filter(|v| !v.is_empty()) {
        return Ok(ids.clone());
    }
    if let Some(handles) = parsed.sites.as_ref().filter(|v| !v.is_empty()) {
        let mut ids = Vec::with_capacity(handles.len());
        for handle in handles {
            let verified = state.plugins.verify_ui_handle(
                plugin_id,
                generation,
                actor_user_id,
                Some("site"),
                handle,
            )?;
            let sid = SiteId::from_str(&verified.entity_id).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_handle", "invalid site handle target")
            })?;
            if !ids.contains(&sid) {
                ids.push(sid);
            }
        }
        return Ok(ids);
    }
    if let Some(handle) = parsed.site_handle.as_deref() {
        let verified = state.plugins.verify_ui_handle(
            plugin_id,
            generation,
            actor_user_id,
            Some("site"),
            handle,
        )?;
        let sid = SiteId::from_str(&verified.entity_id).map_err(|_| {
            PluginServiceError::invalid("plugin.invalid_handle", "invalid site handle target")
        })?;
        return Ok(vec![sid]);
    }
    if let Some(sid) = parsed.site_id {
        return Ok(vec![sid]);
    }
    Err(PluginServiceError::invalid(
        "plugin.invalid_input",
        "site_handle, sites, or site_id is required",
    ))
}

pub(crate) fn validate_indexer_search_command(
    state: &ApiState,
    actor_user_id: UserId,
    plugin: &PluginRecord,
    targets: &[store::plugins::PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    let parsed: IndexerSearchPayload = serde_json::from_value(payload.clone())
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    if parsed.search_term().is_none() {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_input",
            "keyword or query must not be empty",
        ));
    }
    let site_ids = resolve_payload_site_ids(
        state,
        &plugin.plugin_id,
        plugin.generation,
        actor_user_id,
        &parsed,
    )?;
    for sid in site_ids {
        let Some(obs) = state.store.observe_site(sid)? else {
            return Err(PluginServiceError::not_found(
                "site.missing",
                "target site does not exist",
            ));
        };
        if !obs.site().enabled {
            return Err(PluginServiceError::conflict(
                "plugin.site_disabled",
                "target site is disabled",
            ));
        }
        if !super::command_targets::targets_cover_site(targets, sid) {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "command targets do not cover target site",
            ));
        }
    }
    Ok(())
}

pub(crate) fn execute_indexer_search_command(
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
    let Ok(req) = serde_json::from_slice::<IndexerSearchPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let Some(search_term) = req.search_term() else {
        return ExternalEffectStep::Rejected {
            phase: "empty-keyword".into(),
        };
    };
    let actor_id = match ctx.subject {
        store::plugins::PluginSubject::User(u) => *u,
        _ => {
            return ExternalEffectStep::Rejected {
                phase: "user-subject-required".into(),
            };
        }
    };
    let generation = ctx.generation.unwrap_or(0);
    let Ok(site_ids) = resolve_payload_site_ids(state, plugin_id, generation, actor_id, &req)
    else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-site-handle".into(),
        };
    };
    let mut sites: Vec<Site> = Vec::with_capacity(site_ids.len());
    for sid in &site_ids {
        if !super::command_targets::grant_scopes_cover_site(ctx.targets, *sid) {
            return ExternalEffectStep::Rejected {
                phase: "target-mismatch".into(),
            };
        }
        let Ok(Some(site)) = state.store.get_site(*sid) else {
            return ExternalEffectStep::Rejected {
                phase: "site-missing".into(),
            };
        };
        if !site.enabled {
            return ExternalEffectStep::Rejected {
                phase: "site-disabled".into(),
            };
        }
        sites.push(site);
    }
    let outcome = state.indexer.search(&sites, search_term);
    let items: Vec<Value> = outcome
        .torrents
        .into_iter()
        .take(50)
        .map(|it| {
            json!({
                "site_id": it.site_id.to_string(),
                "title": it.title,
                "size_bytes": it.size_bytes,
                "seeders": it.seeders,
                "leechers": it.leechers,
                "published_at": it.upload_time,
                "category": it.category,
                "free": it.free,
                "hr": it.hr,
            })
        })
        .collect();
    let primary_site = site_ids
        .first()
        .map(ToString::to_string)
        .unwrap_or_default();
    let result = serde_json::to_vec(&json!({
        "site_id": primary_site,
        "keyword": search_term,
        "items": items,
    }))
    .ok();
    ExternalEffectStep::Succeeded { result }
}
