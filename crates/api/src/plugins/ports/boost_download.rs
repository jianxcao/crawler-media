use std::collections::HashSet;
use std::str::FromStr;

use domain::{DownloaderId, SiteId};
use plugin_api::{ApiError, BoostSnapshotView, CommandAction, ErrorCode, GuestRole, TaskControl};
use plugin_host::{HostPorts, Invocation};
use serde_json::json;
use store::Store;
use store::plugins::{PluginCommandTarget, PluginGrantScope};

use super::{ActiveAuth, StoreHostPorts, api_err, map_store_error, strip_site_prefix};
use crate::plugins::PluginServiceError;
use crate::plugins::boost::{SnapshotQuery, collect_boost_snapshot_sync};

struct BoostGrantFilter {
    global: bool,
    sites: HashSet<SiteId>,
    downloaders: HashSet<DownloaderId>,
}

fn build_grant_filter(
    grants: &[store::plugins::PluginGrant],
    bound_site: Option<SiteId>,
) -> Result<BoostGrantFilter, ApiError> {
    let mut global = false;
    let mut sites = HashSet::new();
    let mut downloaders = HashSet::new();
    for g in grants.iter().filter(|g| g.capability == "read:boost") {
        match &g.scope {
            PluginGrantScope::Global => global = true,
            PluginGrantScope::Site(sid) => {
                sites.insert(*sid);
            }
            PluginGrantScope::Object { kind, id } => match kind.trim() {
                "pool" => global = true,
                "site" => {
                    let sid = super::parse_site_scope_id(id).ok_or_else(|| {
                        api_err(ErrorCode::PermissionDenied, "invalid read:boost site scope")
                    })?;
                    sites.insert(sid);
                }
                "downloader" => {
                    let dl = super::parse_downloader_scope_id(id).ok_or_else(|| {
                        api_err(
                            ErrorCode::PermissionDenied,
                            "invalid read:boost downloader scope",
                        )
                    })?;
                    downloaders.insert(dl);
                }
                _ => {
                    return Err(api_err(
                        ErrorCode::PermissionDenied,
                        "unsupported read:boost scope kind",
                    ));
                }
            },
        }
    }
    if let Some(bs) = bound_site {
        if global {
            sites.clear();
            sites.insert(bs);
            downloaders.clear();
            global = false;
        } else if !sites.is_empty() {
            sites.retain(|s| *s == bs);
            if sites.is_empty() {
                return Err(api_err(
                    ErrorCode::PermissionDenied,
                    "read:boost grant does not cover bound site",
                ));
            }
        } else if !downloaders.is_empty() {
            sites.insert(bs);
        }
    }
    if !global && sites.is_empty() && downloaders.is_empty() {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "read:boost grant covers no valid scope",
        ));
    }
    Ok(BoostGrantFilter {
        global,
        sites,
        downloaders,
    })
}

fn parse_site_filters(sites: &[String]) -> Result<Vec<SiteId>, ApiError> {
    let mut out = Vec::new();
    for raw in sites {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "global" || trimmed.starts_with("pool:") {
            continue;
        }
        let sid = SiteId::from_str(strip_site_prefix(trimmed))
            .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid site filter UUID"))?;
        if !out.contains(&sid) {
            out.push(sid);
        }
    }
    Ok(out)
}

fn parse_downloader_filters(downloaders: &[String]) -> Result<Vec<DownloaderId>, ApiError> {
    let mut out = Vec::new();
    for raw in downloaders {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let dl_id = DownloaderId::from_str(trimmed)
            .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid downloader filter UUID"))?;
        if !out.contains(&dl_id) {
            out.push(dl_id);
        }
    }
    Ok(out)
}

fn map_boost_service_error(err: PluginServiceError) -> ApiError {
    match err {
        PluginServiceError::NotFound { message, .. } => api_err(ErrorCode::NotFound, &message),
        PluginServiceError::InvalidArgument { message, .. } => {
            api_err(ErrorCode::InvalidArgument, &message)
        }
        PluginServiceError::Forbidden { message, .. } => {
            api_err(ErrorCode::PermissionDenied, &message)
        }
        PluginServiceError::Busy { message, .. } => api_err(ErrorCode::Busy, &message),
        PluginServiceError::Store(e) => map_store_error(e),
        other => api_err(ErrorCode::Conflict, &other.to_string()),
    }
}

pub(super) fn boost_snapshot(
    ports: &StoreHostPorts,
    inv: &Invocation,
    sites: &[String],
    downloaders: &[String],
) -> Result<BoostSnapshotView, ApiError> {
    if !matches!(inv.role, GuestRole::Task | GuestRole::Endpoint) {
        return Err(api_err(
            ErrorCode::WrongContext,
            "boost.snapshot is restricted to task and endpoint contexts",
        ));
    }
    let ctx = ports.lookup(inv)?;
    let authority = ports.runtime_authority(&ctx)?;
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    if !ports.authorized(inv, "read:boost") {
        return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
    }

    let ActiveAuth::Runtime {
        grants,
        invocation_site,
        ..
    } = &ctx.auth
    else {
        return Err(api_err(ErrorCode::WrongContext, "invalid runtime auth"));
    };
    let bound_site = authority
        .site_id()
        .or_else(|| invocation_site.as_ref().map(|(id, _)| *id));
    let grant_filter = build_grant_filter(grants, bound_site)?;

    let req_sites = parse_site_filters(sites)?;
    let req_dls = parse_downloader_filters(downloaders)?;

    if !grant_filter.global {
        if !grant_filter.sites.is_empty()
            && req_sites.iter().any(|s| !grant_filter.sites.contains(s))
        {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "read:boost does not cover requested site",
            ));
        }
        if !grant_filter.downloaders.is_empty()
            && req_dls
                .iter()
                .any(|d| !grant_filter.downloaders.contains(d))
        {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "read:boost does not cover requested downloader",
            ));
        }
        if grant_filter.sites.is_empty() && !req_sites.is_empty() {
            return Err(api_err(
                ErrorCode::PermissionDenied,
                "read:boost is downloader-scoped only",
            ));
        }
    }

    let allowed_sites =
        (!grant_filter.global && !grant_filter.sites.is_empty()).then_some(&grant_filter.sites);
    let allowed_dls = (!grant_filter.global && !grant_filter.downloaders.is_empty())
        .then_some(&grant_filter.downloaders);

    collect_boost_snapshot_sync(
        &ctx.deps.store,
        &ctx.deps.downloader,
        &SnapshotQuery {
            plugin_id: authority.plugin_id(),
            site_ids: &req_sites,
            downloader_ids: &req_dls,
            allowed_sites,
            allowed_downloaders: allowed_dls,
        },
    )
    .map_err(map_boost_service_error)
}

fn serialize_task_control(control: &TaskControl) -> serde_json::Value {
    match control {
        TaskControl::Pause => json!({ "action": "pause" }),
        TaskControl::Resume => json!({ "action": "resume" }),
        TaskControl::Tags(tags) => json!({ "action": "tags", "tags": tags }),
        TaskControl::Category(cat) => json!({ "action": "category", "category": cat }),
        TaskControl::Limits {
            upload: ul,
            download: dl,
        } => {
            json!({ "action": "limits", "upload_limit": ul, "download_limit": dl })
        }
    }
}

fn load_scoped_boost_task_fact(
    store: &Store,
    plugin_id: &str,
    bound_site: Option<SiteId>,
    handle: &str,
    dl_hint: Option<DownloaderId>,
) -> Result<store::download_operations::BoostTaskFactRecord, ApiError> {
    let found = match store
        .find_boost_task_fact_by_handle(plugin_id, handle)
        .map_err(map_store_error)?
    {
        Some(tf) => Some(tf),
        None => match dl_hint {
            Some(dl_id) => store
                .get_boost_task_fact_by_hash(plugin_id, dl_id, handle)
                .map_err(map_store_error)?,
            None => None,
        },
    };
    found
        .filter(|tf| {
            tf.plugin_id == plugin_id
                && bound_site.is_none_or(|bs| tf.site_id == Some(bs))
                && dl_hint.is_none_or(|dl_id| tf.downloader_id == dl_id)
        })
        .ok_or_else(|| api_err(ErrorCode::NotFound, "boost task fact not found"))
}

fn resolve_downloader_add(
    downloader_id: &str,
    download_ref: &str,
    options: &plugin_api::DownloadOptions,
    bound_site: Option<SiteId>,
) -> Result<(Vec<PluginCommandTarget>, Vec<u8>), ApiError> {
    let dl_id = DownloaderId::from_str(downloader_id.trim())
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid downloader_id"))?;
    let download_ref = download_ref.trim();
    if download_ref.is_empty() {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "download_ref must not be empty",
        ));
    }
    if let Some(rel) = options.relative_directory.as_deref()
        && !rel.is_empty()
    {
        hooks::validate_rename_path(rel).map_err(|err| {
            api_err(
                ErrorCode::InvalidArgument,
                &format!("invalid relative_directory: {err}"),
            )
        })?;
    }
    let payload = serde_json::to_vec(&json!({
        "downloader_id": dl_id.to_string(),
        "download_ref": download_ref,
        "site_id": bound_site.map(|s| s.to_string()),
        "relative_directory": options.relative_directory,
        "category": options.category,
        "tags": options.tags,
        "paused": options.paused,
        "upload_limit": options.upload_limit,
        "download_limit": options.download_limit,
        "sequential": options.sequential,
        "first_last_piece": options.first_last_piece,
    }))
    .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
    Ok((vec![PluginCommandTarget::Downloader(dl_id)], payload))
}

fn resolve_downloader_prepare_cleanup(
    store: &Store,
    plugin_id: &str,
    bound_site: Option<SiteId>,
    targets: &[plugin_api::OwnedRevision],
    reason: &str,
) -> Result<(Vec<PluginCommandTarget>, Vec<u8>), ApiError> {
    if targets.len() != 1 || reason.trim().is_empty() {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "prepare-cleanup requires exactly one target and non-empty reason",
        ));
    }
    let mut dl_targets: Vec<PluginCommandTarget> = Vec::new();
    let mut target_items = Vec::with_capacity(targets.len());
    for item in targets {
        let handle = item.handle.trim();
        if handle.is_empty() {
            return Err(api_err(
                ErrorCode::InvalidArgument,
                "target handle must not be empty",
            ));
        }
        let tf = load_scoped_boost_task_fact(store, plugin_id, bound_site, handle, None)?;
        let tgt = PluginCommandTarget::Downloader(tf.downloader_id);
        if !dl_targets.contains(&tgt) {
            dl_targets.push(tgt);
        }
        target_items.push(json!({
            "task_id": tf.task_fact_id,
            "expected_revision": item.revision,
        }));
    }
    let payload = serde_json::to_vec(&json!({
        "targets": target_items,
        "reason": reason,
        "ttl_secs": 600,
    }))
    .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
    Ok((dl_targets, payload))
}

pub(super) fn resolve_downloader_command(
    store: &Store,
    plugin_id: &str,
    bound_site: Option<SiteId>,
    action: &CommandAction,
) -> Result<(Vec<PluginCommandTarget>, Vec<u8>), ApiError> {
    match action {
        CommandAction::DownloaderAdd {
            downloader_id,
            download_ref,
            options,
        } => resolve_downloader_add(downloader_id, download_ref, options, bound_site),
        CommandAction::DownloaderControl { target, control } => {
            let handle = target.handle.trim();
            if handle.is_empty() {
                return Err(api_err(
                    ErrorCode::InvalidArgument,
                    "handle must not be empty",
                ));
            }
            let tf = load_scoped_boost_task_fact(store, plugin_id, bound_site, handle, None)?;
            let mut body = serialize_task_control(control);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("task_fact_id".into(), json!(tf.task_fact_id));
                obj.insert("expected_revision".into(), json!(target.revision));
                obj.insert("downloader_id".into(), json!(tf.downloader_id.to_string()));
            }
            let payload = serde_json::to_vec(&body)
                .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((
                vec![PluginCommandTarget::Downloader(tf.downloader_id)],
                payload,
            ))
        }
        CommandAction::DownloaderDeleteTask {
            downloader_id,
            revision,
            task_handle,
        } => {
            let dl_id = DownloaderId::from_str(downloader_id.trim())
                .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid downloader_id"))?;
            let handle = task_handle.trim();
            if handle.is_empty() {
                return Err(api_err(
                    ErrorCode::InvalidArgument,
                    "task_handle must not be empty",
                ));
            }
            let tf =
                load_scoped_boost_task_fact(store, plugin_id, bound_site, handle, Some(dl_id))?;
            let payload = serde_json::to_vec(&json!({
                "downloader_id": dl_id.to_string(),
                "expected_revision": revision,
                "task_handle": tf.task_fact_id,
            }))
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((vec![PluginCommandTarget::Downloader(dl_id)], payload))
        }
        CommandAction::DownloaderPrepareCleanup { targets, reason } => {
            resolve_downloader_prepare_cleanup(store, plugin_id, bound_site, targets, reason)
        }
        CommandAction::DownloaderDeleteFiles {
            target,
            plan_handle,
        } => {
            let handle = target.handle.trim();
            let plan = plan_handle.trim();
            if handle.is_empty() || plan.is_empty() {
                return Err(api_err(
                    ErrorCode::InvalidArgument,
                    "target handle and plan_handle must not be empty",
                ));
            }
            let tf = load_scoped_boost_task_fact(store, plugin_id, bound_site, handle, None)?;
            let plan_tf = store
                .get_boost_task_fact_by_plan(plan)
                .map_err(map_store_error)?
                .filter(|p| {
                    p.plugin_id == plugin_id
                        && bound_site.is_none_or(|bs| p.site_id == Some(bs))
                        && p.task_fact_id == tf.task_fact_id
                })
                .ok_or_else(|| api_err(ErrorCode::NotFound, "boost cleanup plan not found"))?;
            let payload = serde_json::to_vec(&json!({
                "task_id": plan_tf.task_fact_id,
                "revision": target.revision,
                "host_file_plan_handle": plan,
            }))
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((
                vec![PluginCommandTarget::Downloader(plan_tf.downloader_id)],
                payload,
            ))
        }
        _ => Err(api_err(
            ErrorCode::Unsupported,
            "unsupported command action",
        )),
    }
}
