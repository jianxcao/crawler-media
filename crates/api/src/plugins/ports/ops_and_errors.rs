use std::str::FromStr;

use domain::SiteId;
use plugin_api::{
    ApiError, CommandRequest, ErrorCode, GuestRole, OperationPage, OperationState, OperationView,
    PageRequest, StateEntry,
};
use plugin_host::HostPorts;
use sha2::{Digest, Sha256};
use store::StoreError;
use store::plugins::{
    CanonicalPluginRequest, PluginCommandTarget, PluginError, PluginOperation,
    PluginOperationState, PluginStateEntry,
};

use super::StoreHostPorts;
use crate::plugins::{PluginServiceError, SubmitPluginCommand};

pub(super) fn operations_get(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    id: &str,
) -> Result<OperationView, ApiError> {
    let ctx = ports.lookup(inv)?;
    let authority = ports.runtime_authority(&ctx)?;
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    let op_id = uuid::Uuid::parse_str(id)
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid operation id"))?;
    let op = ctx
        .deps
        .store
        .get_plugin_operation(&ctx.subject, op_id)
        .map_err(map_store_error)?
        .ok_or_else(|| api_err(ErrorCode::NotFound, "operation not found"))?;
    Ok(operation_dto(&op))
}

pub(super) fn operations_list(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    page: PageRequest,
) -> Result<OperationPage, ApiError> {
    let ctx = ports.lookup(inv)?;
    let authority = ports.runtime_authority(&ctx)?;
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    let after = page
        .cursor
        .as_deref()
        .map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid operation cursor"))?;
    let limit = (page.limit.max(1) as usize).min(100);
    let ops = ctx
        .deps
        .store
        .list_plugin_operations(&ctx.subject, after, Some(limit))
        .map_err(map_store_error)?;
    let next_cursor = if ops.len() == limit {
        ops.last().map(|op| op.operation_id.to_string())
    } else {
        None
    };
    Ok(OperationPage {
        items: ops.iter().map(operation_dto).collect(),
        next_cursor,
    })
}

pub(super) fn commands_submit(
    ports: &StoreHostPorts,
    inv: &plugin_host::Invocation,
    req: CommandRequest,
) -> Result<String, ApiError> {
    if !matches!(inv.role, GuestRole::Task | GuestRole::Endpoint) {
        return Err(api_err(
            ErrorCode::WrongContext,
            "commands.submit is forbidden in this role",
        ));
    }
    let ctx = ports.lookup(inv)?;
    if !ctx.role.allows_command_submit() {
        return Err(api_err(
            ErrorCode::WrongContext,
            "commands.submit is forbidden in this role",
        ));
    }
    ctx.deps.ensure_write_admitted()?;
    let authority = ports.runtime_authority(&ctx)?.clone();
    let bound_site = authority.site_id().or_else(|| match &ctx.auth {
        super::ActiveAuth::Runtime {
            invocation_site, ..
        } => invocation_site.as_ref().map(|(id, _)| *id),
        super::ActiveAuth::Lifecycle { .. } => None,
    });
    let requested_kind = req.action.kind_name().to_string();
    let cap = format!("command:{requested_kind}");
    if !ports.authorized(inv, &cap) {
        return Err(api_err(ErrorCode::PermissionDenied, "permission denied"));
    }
    let (targets, payload) = resolve_command_payload(
        &ctx.deps.store,
        authority.plugin_id(),
        bound_site,
        &req.action,
    )?;
    if !authorized_for_targets(&ctx, &cap, &targets) {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "permission denied for command target",
        ));
    }
    let kind = if authorized_for_targets_inner(&ctx, &cap, &targets, true) {
        requested_kind
    } else {
        "site.maintenance".to_string()
    };
    let expected_revision = req
        .expected_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "revision out of range"))?;
    let body_digest: [u8; 32] = Sha256::digest(&payload).into();
    let path = format!("/plugins/{}/commands/{kind}", authority.plugin_id());
    let accepted = crate::plugins::commands::submit_command_with_deps(
        &ctx.deps.store,
        &ctx.deps.jobs,
        &ctx.deps.effects,
        SubmitPluginCommand {
            role: ctx.role,
            authority,
            subject: ctx.subject,
            kind,
            targets,
            idempotency_key: req.idempotency_key,
            request: CanonicalPluginRequest {
                method: "GUEST-CMD".into(),
                path,
                body_digest,
            },
            expected_revision,
            cause: ctx.cause,
            payload,
        },
    )
    .map_err(map_service_error)?;
    Ok(accepted.operation.operation_id.to_string())
}

pub(super) fn state_entry_dto(entry: PluginStateEntry) -> StateEntry {
    StateEntry {
        key: entry.key,
        value: entry.value.as_bytes().to_vec(),
        etag: entry.revision.to_string(),
    }
}

fn operation_dto(op: &PluginOperation) -> OperationView {
    let state = match op.state {
        PluginOperationState::Queued => OperationState::Queued,
        PluginOperationState::Running => OperationState::Running,
        PluginOperationState::Waiting => OperationState::Waiting,
        PluginOperationState::Blocked => OperationState::Blocked,
        PluginOperationState::Succeeded => OperationState::Succeeded,
        PluginOperationState::Rejected => OperationState::Rejected,
        PluginOperationState::Deferred => OperationState::Deferred,
        PluginOperationState::Failed => OperationState::Failed,
        PluginOperationState::Cancelled => OperationState::Cancelled,
        PluginOperationState::Unknown => OperationState::Unknown,
    };
    OperationView {
        id: op.operation_id.to_string(),
        state,
        revision: u64::try_from(op.revision).unwrap_or(0),
        next_run_at: op.next_run_at.and_then(|ts| u64::try_from(ts).ok()),
    }
}

pub(super) fn parse_etag(tag: &str, allow_zero: bool) -> Result<i64, ApiError> {
    let Ok(rev) = tag.parse::<i64>() else {
        return Err(api_err(ErrorCode::Conflict, "invalid etag"));
    };
    if tag != rev.to_string() || rev < 0 || (!allow_zero && rev == 0) {
        return Err(api_err(ErrorCode::Conflict, "invalid etag"));
    }
    Ok(rev)
}

fn target_matches_grant_scope(
    target: &PluginCommandTarget,
    scope: &store::plugins::PluginGrantScope,
) -> bool {
    match (target, scope) {
        (_, store::plugins::PluginGrantScope::Global) => true,
        (PluginCommandTarget::Global, _) => false,
        (PluginCommandTarget::Site(site_id), store::plugins::PluginGrantScope::Site(s)) => {
            *s == *site_id
        }
        (
            PluginCommandTarget::Site(site_id),
            store::plugins::PluginGrantScope::Object { kind, id },
        ) => kind.trim() == "site" && super::parse_site_scope_id(id) == Some(*site_id),
        (
            PluginCommandTarget::Downloader(dl_id),
            store::plugins::PluginGrantScope::Object { kind, id },
        ) => kind.trim() == "downloader" && super::parse_downloader_scope_id(id) == Some(*dl_id),
        (
            PluginCommandTarget::Library(lib_id),
            store::plugins::PluginGrantScope::Object { kind, id },
        ) => kind.trim() == "library" && super::parse_library_scope_id(id) == Some(*lib_id),
        (
            PluginCommandTarget::Subscribe(sub_id),
            store::plugins::PluginGrantScope::Object { kind, id },
        ) => kind.trim() == "subscribe" && super::parse_subscribe_scope_id(id) == Some(*sub_id),
        _ => false,
    }
}

fn authorized_for_targets_inner(
    ctx: &super::ActiveInvocation,
    cap: &str,
    targets: &[PluginCommandTarget],
    exact_only: bool,
) -> bool {
    let super::ActiveAuth::Runtime {
        grants,
        authority,
        invocation_site,
    } = &ctx.auth
    else {
        return false;
    };
    let bound_site = authority
        .site_id()
        .or_else(|| invocation_site.as_ref().map(|(id, _)| *id));
    targets.iter().all(|target| {
        if let Some(bs) = bound_site {
            let allowed_under_bound_site = match target {
                PluginCommandTarget::Site(s) => *s == bs,
                PluginCommandTarget::Downloader(_) => cap.starts_with("command:downloader."),
                _ => false,
            };
            if !allowed_under_bound_site {
                return false;
            }
        }
        grants.iter().any(|g| {
            let cap_ok = if exact_only {
                g.capability == cap
            } else {
                super::authz::command_capability_matches(cap, &g.capability)
            };
            cap_ok && target_matches_grant_scope(target, &g.scope)
        })
    })
}

fn authorized_for_targets(
    ctx: &super::ActiveInvocation,
    cap: &str,
    targets: &[PluginCommandTarget],
) -> bool {
    authorized_for_targets_inner(ctx, cap, targets, false)
}

pub(super) fn resolve_command_payload(
    store: &store::Store,
    plugin_id: &str,
    bound_site: Option<SiteId>,
    action: &plugin_api::CommandAction,
) -> Result<(Vec<PluginCommandTarget>, Vec<u8>), ApiError> {
    match action {
        plugin_api::CommandAction::SiteMaintenance {
            site_handle,
            action: maint,
        } => {
            let target = parse_target(site_handle)?;
            let action_name = match maint {
                plugin_api::MaintenanceAction::VerifySession => "verify-session",
                plugin_api::MaintenanceAction::CheckIn => "check-in",
            };
            let payload_json = match target {
                PluginCommandTarget::Site(site_id) => serde_json::json!({
                    "site_id": site_id,
                    "action": action_name,
                }),
                PluginCommandTarget::Global => serde_json::json!({
                    "action": action_name,
                }),
                _ => {
                    return Err(api_err(
                        ErrorCode::InvalidArgument,
                        "site maintenance requires site target",
                    ));
                }
            };
            let payload = serde_json::to_vec(&payload_json)
                .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((vec![target], payload))
        }
        plugin_api::CommandAction::ScheduleSet {
            task_id,
            interval_secs,
            enabled,
            expected_revision,
        } => {
            let payload = serde_json::to_vec(&serde_json::json!({
                "task_id": task_id,
                "interval_secs": interval_secs,
                "enabled": enabled,
                "expected_revision": expected_revision,
            }))
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((vec![PluginCommandTarget::Global], payload))
        }
        plugin_api::CommandAction::ScheduleDisable { handle, revision } => {
            let payload = serde_json::to_vec(&serde_json::json!({
                "handle": handle,
                "revision": revision,
            }))
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((vec![PluginCommandTarget::Global], payload))
        }
        plugin_api::CommandAction::DownloaderAdd { .. }
        | plugin_api::CommandAction::DownloaderControl { .. }
        | plugin_api::CommandAction::DownloaderDeleteTask { .. }
        | plugin_api::CommandAction::DownloaderPrepareCleanup { .. }
        | plugin_api::CommandAction::DownloaderDeleteFiles { .. } => {
            super::boost_download::resolve_downloader_command(store, plugin_id, bound_site, action)
        }
        plugin_api::CommandAction::Other { kind, summary } => {
            let payload = serde_json::to_vec(&serde_json::json!({
                "kind": kind,
                "summary": summary,
            }))
            .map_err(|e| api_err(ErrorCode::InvalidArgument, &e.to_string()))?;
            Ok((vec![PluginCommandTarget::Global], payload))
        }
    }
}

fn parse_target(handle: &str) -> Result<PluginCommandTarget, ApiError> {
    if handle == "global" {
        return Ok(PluginCommandTarget::Global);
    }
    let raw = super::strip_site_prefix(handle);
    let site_id = SiteId::from_str(raw)
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid site handle"))?;
    Ok(PluginCommandTarget::Site(site_id))
}

pub(crate) fn map_store_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Plugin(pe) => match pe {
            PluginError::Denied => api_err(ErrorCode::PermissionDenied, &pe.to_string()),
            PluginError::StaleAuthority => api_err(ErrorCode::StaleGeneration, &pe.to_string()),
            PluginError::NotFound => api_err(ErrorCode::NotFound, &pe.to_string()),
            PluginError::Conflict
            | PluginError::CauseLoop
            | PluginError::PackageConflict
            | PluginError::EffectDispositionRequired
            | PluginError::UnknownEffect
            | PluginError::IdempotencyConflict
            | PluginError::OperationActive(_)
            | PluginError::StateSchemaMismatch => api_err(ErrorCode::Conflict, &pe.to_string()),
            PluginError::QuotaExceeded => api_err(ErrorCode::QuotaExceeded, &pe.to_string()),
            PluginError::Invalid(_) => api_err(ErrorCode::InvalidArgument, &pe.to_string()),
            PluginError::NotDrained => api_err(ErrorCode::Busy, &pe.to_string()),
        },
        StoreError::WriteBusy => api_err(ErrorCode::Busy, "store writer busy"),
        other => api_err(ErrorCode::Conflict, &other.to_string()),
    }
}

fn map_service_error(error: PluginServiceError) -> ApiError {
    match error {
        PluginServiceError::WrongContext => {
            api_err(ErrorCode::WrongContext, "wrong plugin command context")
        }
        PluginServiceError::UnsupportedCommand(kind) => api_err(
            ErrorCode::Unsupported,
            &format!("unsupported command: {kind}"),
        ),
        PluginServiceError::Store(err) => map_store_error(err),
        PluginServiceError::Jobs(err) => api_err(ErrorCode::Busy, &err.to_string()),
        other => api_err(ErrorCode::InvalidArgument, &other.to_string()),
    }
}

pub(crate) fn api_err(code: ErrorCode, message: &str) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}
