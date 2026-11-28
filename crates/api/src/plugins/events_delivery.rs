#[path = "events_delivery_progress.rs"]
mod progress;

use std::str::FromStr;
use std::time::Instant;

use domain::{SiteId, SubscribeId};
use hooks::{HookStepTrace, HookTrace};
use plugin_api::{
    DownloadAfterEvent, ErrorCode, Event, EventPayload, Failure, HOOK_WORLD, HookContext,
    SiteCheckinAfterEvent, TransferCompletedEvent,
};
use plugin_package::PluginManifest;
use store::Store;
use store::plugin_events::{
    ClaimedOutboxDelivery, MarkDeadLetterInput, MarkDeliveredInput, OutboxDb, OutboxEventRecord,
    ScheduleRetryInput,
};
use store::plugins::{
    PluginEffectObservation, PluginLifecycle, PluginOperationState, PluginRecord, RecordPluginAudit,
};
use uuid::Uuid;

use super::events::{
    EVENT_NOTIFY_TIMEOUT_MS, EVENT_RETRY_BACKOFF_MS, EventDispatchContext, FanoutReadOp,
    MAX_EVENT_ATTEMPTS, extract_event_scope, is_transient_store_error,
    plugin_holds_dual_event_grants_for_event, store_error_kind,
};
use super::hooks_runtime::{PolicyAuthority, resolve_wasm_hook_invoker};
use super::ports::strip_site_prefix;
use super::wasm_runner::resolve_plugin_trust;
use progress::{DeliveryTarget, execute_target_components};

enum ResolveTargetsError {
    Transient {
        error_kind: &'static str,
        reason: String,
    },
    Rejected(String),
    Failed(String),
}

impl ResolveTargetsError {
    fn from_store(context: &str, err: store::StoreError) -> Self {
        if is_transient_store_error(&err) {
            Self::Transient {
                error_kind: store_error_kind(&err),
                reason: format!("{context}: {err}"),
            }
        } else {
            Self::Failed(format!("{context}: {err}"))
        }
    }
}

fn resolve_event_authority_mode(
    store: &Store,
    plugins: &super::PluginService,
    plugin: &PluginRecord,
    credential_bound: bool,
    event: &OutboxEventRecord,
) -> Result<PolicyAuthority, ResolveTargetsError> {
    if !credential_bound {
        let authority = store
            .issue_plugin_state_authority(plugin, None)
            .map_err(|e| ResolveTargetsError::from_store("failed to issue plugin authority", e))?;
        return Ok(PolicyAuthority::Fixed {
            authority,
            credential_epoch: None,
        });
    }
    let (resolved_site_id, _, _) = extract_event_scope(event);
    let raw_site = resolved_site_id.as_deref().ok_or_else(|| {
        ResolveTargetsError::Rejected("credential-bound plugin event missing site_id".to_string())
    })?;
    let sid = SiteId::from_str(strip_site_prefix(raw_site)).map_err(|_| {
        ResolveTargetsError::Rejected(format!("invalid event site_id `{raw_site}`"))
    })?;
    let obs = plugins
        .check_fanout_read_fault(FanoutReadOp::ObserveSite { site_id: raw_site })
        .and_then(|()| store.observe_site(sid))
        .map_err(|e| ResolveTargetsError::from_store("failed to observe site", e))?
        .ok_or_else(|| ResolveTargetsError::Rejected("event site not found".to_string()))?;
    let epoch = obs.credential_epoch().storage_key().to_string();
    let authority = store
        .issue_plugin_state_authority(plugin, Some(&obs))
        .map_err(|e| {
            ResolveTargetsError::from_store("failed to issue site-bound plugin authority", e)
        })?;
    Ok(PolicyAuthority::Fixed {
        authority,
        credential_epoch: Some(epoch),
    })
}

fn resolve_delivery_targets(
    ctx: &EventDispatchContext,
    plugin_id: &str,
    event: &OutboxEventRecord,
) -> Result<(PluginRecord, Vec<DeliveryTarget>), ResolveTargetsError> {
    let event_kind = event.event_kind.as_str();
    let plugin = ctx
        .plugins
        .check_fanout_read_fault(FanoutReadOp::GetPlugin { plugin_id })
        .and_then(|()| ctx.store.get_plugin(plugin_id))
        .map_err(|e| ResolveTargetsError::from_store("failed to load plugin", e))?
        .ok_or_else(|| ResolveTargetsError::Rejected("plugin not installed".to_string()))?;
    if plugin.lifecycle != PluginLifecycle::Enabled || !plugin.desired_enabled {
        return Err(ResolveTargetsError::Rejected(
            "plugin is disabled".to_string(),
        ));
    }
    let has_grants =
        plugin_holds_dual_event_grants_for_event(&ctx.store, &ctx.plugins, plugin_id, event)
            .map_err(|e| ResolveTargetsError::from_store("failed to load plugin grants", e))?;
    if !has_grants {
        return Err(ResolveTargetsError::Rejected(format!(
            "missing scoped hooks:{event_kind} or events:{event_kind} grant"
        )));
    }
    let version = ctx
        .plugins
        .check_fanout_read_fault(FanoutReadOp::PluginVersion {
            plugin_id: &plugin.plugin_id,
        })
        .and_then(|()| {
            ctx.store
                .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)
        })
        .map_err(|e| ResolveTargetsError::from_store("failed to load plugin version", e))?
        .ok_or_else(|| {
            ResolveTargetsError::Rejected("active plugin version missing".to_string())
        })?;
    let manifest = serde_json::from_str::<PluginManifest>(&version.manifest_json)
        .map_err(|e| ResolveTargetsError::Failed(format!("invalid manifest JSON: {e}")))?;
    let authority_mode = resolve_event_authority_mode(
        &ctx.store,
        &ctx.plugins,
        &plugin,
        version.credential_bound,
        event,
    )?;
    let host = ctx
        .plugins
        .wasm_host()
        .map_err(|e| ResolveTargetsError::Failed(format!("wasm host unavailable: {e}")))?;
    let trust = resolve_plugin_trust(
        &plugin.plugin_id,
        &version.signature_status,
        &ctx.deps.admin_trusted_plugins,
    );
    let hook_cap = format!("hooks:{event_kind}");
    let event_cap = format!("events:{event_kind}");
    let mut targets: Vec<DeliveryTarget> = Vec::new();
    for comp in &manifest.components {
        if comp.world != HOOK_WORLD {
            continue;
        }
        let Some(hook) = comp.hooks.iter().find(|h| h.id == event_kind) else {
            continue;
        };
        if !comp.permissions.iter().any(|p| p == &hook_cap)
            || !comp.permissions.iter().any(|p| p == &event_cap)
        {
            continue;
        }
        let invoker = resolve_wasm_hook_invoker(
            &ctx.plugins,
            &ctx.deps,
            &host,
            &plugin,
            &version,
            comp,
            event_kind,
            authority_mode.clone(),
            trust,
            EVENT_NOTIFY_TIMEOUT_MS,
        )
        .ok_or_else(|| {
            ResolveTargetsError::Failed(format!(
                "failed to load policy component `{}` for hook {event_kind}",
                comp.id
            ))
        })?;
        targets.push((hook.priority, comp.id.clone(), invoker));
    }
    if targets.is_empty() {
        return Err(ResolveTargetsError::Rejected(format!(
            "no policy component declared hook {event_kind}"
        )));
    }
    targets.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok((plugin, targets))
}

fn build_guest_event(item: &ClaimedOutboxDelivery, plugin: &PluginRecord) -> Result<Event, String> {
    let payload = match item.event.event_kind.as_str() {
        "site.checkin.after" => EventPayload::SiteCheckinAfter(
            serde_json::from_str::<SiteCheckinAfterEvent>(&item.event.payload_json)
                .map_err(|e| format!("invalid site.checkin.after payload: {e}"))?,
        ),
        "download.after" => EventPayload::DownloadAfter(
            serde_json::from_str::<DownloadAfterEvent>(&item.event.payload_json)
                .map_err(|e| format!("invalid download.after payload: {e}"))?,
        ),
        "transfer.completed" => EventPayload::TransferCompleted(
            serde_json::from_str::<TransferCompletedEvent>(&item.event.payload_json)
                .map_err(|e| format!("invalid transfer.completed payload: {e}"))?,
        ),
        other => return Err(format!("unsupported event kind: {other}")),
    };
    let (site_id, library_id, subscribe_id) = extract_event_scope(&item.event);
    let handles: Vec<String> = serde_json::from_str(&item.event.handles_json).unwrap_or_default();
    let ctx = HookContext {
        invocation_id: format!(
            "evt-{}-{}",
            item.delivery.attempt_count,
            Uuid::new_v4().simple()
        ),
        cause_id: item.event.cause_id.clone(),
        plugin_id: plugin.plugin_id.clone(),
        generation: u64::try_from(plugin.generation).unwrap_or(1),
        config_revision: u64::try_from(plugin.config_revision).unwrap_or(1),
        subject: item.event.subject.clone(),
        site_id,
        subscribe_id,
        library_id,
        deadline_ms: EVENT_NOTIFY_TIMEOUT_MS,
        cancelled: false,
    };
    Ok(Event {
        schema_version: u32::try_from(item.event.schema_version).unwrap_or(1),
        context: ctx,
        id: item.event.event_id.clone(),
        kind: item.event.event_kind.clone(),
        occurred_at: u64::try_from(item.event.occurred_at.max(0)).unwrap_or(0),
        handles,
        revision: u64::try_from(item.event.entity_revision).unwrap_or(1),
        payload,
    })
}

fn record_event_trace(
    plugins: &super::PluginService,
    item: &ClaimedOutboxDelivery,
    invocation_id: String,
    steps: Vec<HookStepTrace>,
    decision: &str,
    reason: Option<String>,
    duration_ms: u64,
) {
    let trace = HookTrace {
        id: invocation_id,
        hook_id: item.event.event_kind.clone(),
        workflow_id: item.event.cause_id.clone(),
        input_revision: u64::try_from(item.event.entity_revision).unwrap_or(1),
        steps,
        decision: decision.to_string(),
        reason,
        duration_ms,
    };
    if item.event.event_kind == "site.checkin.after" {
        plugins.record_checkin_trace(trace);
    } else if let Some(sub_id) = item
        .event
        .subscribe_id
        .as_deref()
        .and_then(|s| s.parse::<SubscribeId>().ok())
    {
        plugins.record_subscribe_trace(sub_id, trace);
    }
}

fn append_delivery_audit(
    store: &Store,
    plugin_id: &str,
    generation: Option<i64>,
    event_kind: &str,
    cause_id: &str,
    op_state: PluginOperationState,
    phase: String,
) {
    if let Err(err) = store.append_plugin_audit(&RecordPluginAudit {
        plugin_id: plugin_id.to_string(),
        generation,
        action: event_kind.to_string(),
        method: "EVENT".to_string(),
        path: format!("/events/{event_kind}"),
        cause_id: cause_id.to_string(),
        state: op_state,
        phase,
        effect: PluginEffectObservation::NoEffect,
    }) {
        tracing::warn!(
            plugin_id,
            event_kind,
            error = %err,
            "Failed to persist plugin event audit"
        );
    }
}

fn classify_audit_outcome(failure: &Failure, reason: &str) -> (PluginOperationState, String) {
    match failure {
        Failure::Guest(api) if api.code == ErrorCode::WrongContext => (
            PluginOperationState::Rejected,
            format!("rejected: wrong_context: {reason}"),
        ),
        Failure::Guest(api)
            if matches!(
                api.code,
                ErrorCode::PermissionDenied | ErrorCode::InvalidArgument
            ) =>
        {
            (
                PluginOperationState::Rejected,
                format!("rejected: {reason}"),
            )
        }
        _ => (PluginOperationState::Failed, format!("failed: {reason}")),
    }
}

fn retry_delay_ms(attempt_count: i64, retry_after_ms: Option<u64>) -> i64 {
    if let Some(ms) = retry_after_ms {
        return i64::try_from(ms).unwrap_or(60_000).clamp(100, 60_000);
    }
    let idx = usize::try_from(attempt_count.saturating_sub(1))
        .unwrap_or(0)
        .min(EVENT_RETRY_BACKOFF_MS.len() - 1);
    EVENT_RETRY_BACKOFF_MS[idx]
}

#[allow(clippy::too_many_arguments)]
fn dead_letter_pre_delivery_error(
    store: &Store,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    generation: Option<i64>,
    claim_nonce: String,
    op_state: PluginOperationState,
    phase: String,
    reason: String,
    clamped_now: i64,
) -> bool {
    let _ = store.mark_dead_letter(
        db,
        &MarkDeadLetterInput {
            event_id: item.event.event_id.clone(),
            plugin_id: item.delivery.plugin_id.clone(),
            claim_nonce,
            error: reason,
            now_ms: clamped_now,
        },
    );
    append_delivery_audit(
        store,
        &item.delivery.plugin_id,
        generation,
        &item.event.event_kind,
        &item.event.cause_id,
        op_state,
        phase,
    );
    false
}

pub(super) fn deliver_claimed_item(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: ClaimedOutboxDelivery,
    now_ms: i64,
    batch_started: Instant,
    renew_before_first: bool,
) -> bool {
    let clamped_now = now_ms.max(0);
    let item_started = Instant::now();
    let plugin_id = item.delivery.plugin_id.clone();
    let claim_nonce = item.claim_nonce.clone();
    let pre_now =
        clamped_now.saturating_add(i64::try_from(batch_started.elapsed().as_millis()).unwrap_or(0));

    let (plugin, targets) = match resolve_delivery_targets(ctx, &plugin_id, &item.event) {
        Ok(found) => found,
        Err(ResolveTargetsError::Transient { error_kind, reason }) => {
            return settle_pre_delivery_transient(
                ctx,
                db,
                &item,
                claim_nonce,
                error_kind,
                reason,
                pre_now,
            );
        }
        Err(ResolveTargetsError::Rejected(reason)) => {
            return dead_letter_resolve_error(
                &ctx.store,
                db,
                &item,
                claim_nonce,
                PluginOperationState::Rejected,
                "rejected",
                reason,
                pre_now,
            );
        }
        Err(ResolveTargetsError::Failed(reason)) => {
            return dead_letter_resolve_error(
                &ctx.store,
                db,
                &item,
                claim_nonce,
                PluginOperationState::Failed,
                "failed",
                reason,
                pre_now,
            );
        }
    };

    let guest_event = match build_guest_event(&item, &plugin) {
        Ok(ev) => ev,
        Err(reason) => {
            let phase = format!("failed: {reason}");
            return dead_letter_pre_delivery_error(
                &ctx.store,
                db,
                &item,
                Some(plugin.generation),
                claim_nonce,
                PluginOperationState::Failed,
                phase,
                reason,
                pre_now,
            );
        }
    };

    let inv_id = guest_event.context.invocation_id.clone();
    let outcome = execute_target_components(
        ctx,
        db,
        &item,
        &plugin,
        &guest_event,
        targets,
        batch_started,
        clamped_now,
        renew_before_first,
    );
    if outcome.claim_lost {
        return false;
    }

    let item_elapsed_ms = u64::try_from(item_started.elapsed().as_millis()).unwrap_or(0);
    let settle_now =
        clamped_now.saturating_add(i64::try_from(batch_started.elapsed().as_millis()).unwrap_or(0));
    finish_delivery_outcome(
        ctx,
        db,
        item,
        &plugin,
        inv_id,
        claim_nonce,
        outcome,
        item_elapsed_ms,
        settle_now,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_delivery_outcome(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: ClaimedOutboxDelivery,
    plugin: &PluginRecord,
    inv_id: String,
    claim_nonce: String,
    outcome: progress::MultiTargetOutcome,
    elapsed_ms: u64,
    settle_now: i64,
) -> bool {
    if let Some((failure, reason, retry_after_ms)) = outcome.retryable_failure {
        record_event_trace(
            &ctx.plugins,
            &item,
            inv_id,
            outcome.steps,
            "error",
            Some(reason.clone()),
            elapsed_ms,
        );
        return settle_notify_failure(
            ctx,
            db,
            item,
            plugin,
            claim_nonce,
            &failure,
            reason,
            retry_after_ms,
            settle_now,
        );
    }

    if let Some((failure, reason)) = outcome.permanent_failure {
        record_event_trace(
            &ctx.plugins,
            &item,
            inv_id,
            outcome.steps,
            "error",
            Some(reason.clone()),
            elapsed_ms,
        );
        return dead_letter_failure(
            &ctx.store,
            db,
            &item,
            plugin,
            claim_nonce,
            &failure,
            &reason,
            settle_now,
        );
    }

    settle_notify_delivered(
        ctx,
        db,
        item,
        plugin,
        inv_id,
        outcome.steps,
        claim_nonce,
        elapsed_ms,
        settle_now,
    )
}

#[allow(clippy::too_many_arguments)]
fn settle_notify_delivered(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: ClaimedOutboxDelivery,
    plugin: &PluginRecord,
    inv_id: String,
    steps: Vec<HookStepTrace>,
    claim_nonce: String,
    duration_ms: u64,
    clamped_now: i64,
) -> bool {
    let plugin_id = item.delivery.plugin_id.clone();
    let event_id = item.event.event_id.clone();
    let event_kind = item.event.event_kind.clone();
    if let Err(err) = ctx.store.mark_delivered(
        db,
        &MarkDeliveredInput {
            event_id: event_id.clone(),
            plugin_id: plugin_id.clone(),
            claim_nonce,
            now_ms: clamped_now,
        },
    ) {
        tracing::warn!(event_id, plugin_id, error = %err, "Failed to mark outbox event delivered");
    }
    record_event_trace(
        &ctx.plugins,
        &item,
        inv_id,
        steps,
        "notified",
        None,
        duration_ms,
    );
    append_delivery_audit(
        &ctx.store,
        &plugin_id,
        Some(plugin.generation),
        &event_kind,
        &item.event.cause_id,
        PluginOperationState::Succeeded,
        "delivered".to_string(),
    );
    false
}

#[allow(clippy::too_many_arguments)]
fn settle_notify_failure(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: ClaimedOutboxDelivery,
    plugin: &PluginRecord,
    claim_nonce: String,
    failure: &Failure,
    reason: String,
    retry_after: Option<u64>,
    clamped_now: i64,
) -> bool {
    let plugin_id = item.delivery.plugin_id.clone();
    let event_id = item.event.event_id.clone();
    if item.delivery.attempt_count >= MAX_EVENT_ATTEMPTS {
        return dead_letter_failure(
            &ctx.store,
            db,
            &item,
            plugin,
            claim_nonce,
            failure,
            &reason,
            clamped_now,
        );
    }
    let delay = retry_delay_ms(item.delivery.attempt_count, retry_after);
    let next_attempt = clamped_now.saturating_add(delay);
    tracing::warn!(
        event_id = %event_id,
        plugin_id = %plugin_id,
        attempt = item.delivery.attempt_count,
        next_attempt_at = next_attempt,
        reason = %reason,
        "Plugin event delivery failed; scheduling retry"
    );
    if let Err(err) = ctx.store.schedule_retry(
        db,
        &ScheduleRetryInput {
            event_id,
            plugin_id,
            claim_nonce,
            next_attempt_at: next_attempt,
            error: reason,
            now_ms: clamped_now,
        },
    ) {
        tracing::warn!(error = %err, "Failed to schedule outbox event retry");
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn dead_letter_failure(
    store: &Store,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    plugin: &PluginRecord,
    claim_nonce: String,
    failure: &Failure,
    reason: &str,
    now_ms: i64,
) -> bool {
    tracing::warn!(
        event_id = %item.event.event_id,
        plugin_id = %plugin.plugin_id,
        attempt = item.delivery.attempt_count,
        reason = %reason,
        "Plugin event delivery moved to dead_letter"
    );
    if let Err(err) = store.mark_dead_letter(
        db,
        &MarkDeadLetterInput {
            event_id: item.event.event_id.clone(),
            plugin_id: plugin.plugin_id.clone(),
            claim_nonce,
            error: reason.to_string(),
            now_ms,
        },
    ) {
        tracing::warn!(error = %err, "Failed to mark outbox event dead_letter");
    }
    let (op_state, phase) = classify_audit_outcome(failure, reason);
    append_delivery_audit(
        store,
        &plugin.plugin_id,
        Some(plugin.generation),
        &item.event.event_kind,
        &item.event.cause_id,
        op_state,
        phase,
    );
    false
}

#[allow(clippy::too_many_arguments)]
fn dead_letter_resolve_error(
    store: &Store,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    claim_nonce: String,
    op_state: PluginOperationState,
    prefix: &str,
    reason: String,
    now_ms: i64,
) -> bool {
    let generation = store
        .get_plugin(&item.delivery.plugin_id)
        .ok()
        .flatten()
        .map(|p| p.generation);
    let phase = format!("{prefix}: {reason}");
    dead_letter_pre_delivery_error(
        store,
        db,
        item,
        generation,
        claim_nonce,
        op_state,
        phase,
        reason,
        now_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn settle_pre_delivery_transient(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    claim_nonce: String,
    error_kind: &'static str,
    reason: String,
    now_ms: i64,
) -> bool {
    let event_id = item.event.event_id.clone();
    let plugin_id = item.delivery.plugin_id.clone();
    if item.delivery.attempt_count >= MAX_EVENT_ATTEMPTS {
        tracing::warn!(
            db = db.as_str(),
            event_id = %event_id,
            plugin_id = %plugin_id,
            attempt = item.delivery.attempt_count,
            error_kind,
            reason = %reason,
            "Transient store read error exhausted max attempts before plugin event delivery; moving to dead_letter"
        );
        let generation = ctx
            .store
            .get_plugin(&plugin_id)
            .ok()
            .flatten()
            .map(|p| p.generation);
        let phase = format!("failed: {reason}");
        return dead_letter_pre_delivery_error(
            &ctx.store,
            db,
            item,
            generation,
            claim_nonce,
            PluginOperationState::Failed,
            phase,
            reason,
            now_ms,
        );
    }
    let delay = retry_delay_ms(item.delivery.attempt_count, None);
    let next_attempt = now_ms.saturating_add(delay);
    tracing::warn!(
        db = db.as_str(),
        event_id = %event_id,
        plugin_id = %plugin_id,
        attempt = item.delivery.attempt_count,
        error_kind,
        next_attempt_at = next_attempt,
        reason = %reason,
        "Transient store read error before plugin event delivery; scheduling retry"
    );
    if let Err(err) = ctx.store.schedule_retry(
        db,
        &ScheduleRetryInput {
            event_id,
            plugin_id,
            claim_nonce,
            next_attempt_at: next_attempt,
            error: reason,
            now_ms,
        },
    ) {
        tracing::warn!(
            db = db.as_str(),
            error_kind = store_error_kind(&err),
            error = %err,
            "Failed to schedule outbox event retry after transient pre-delivery read error"
        );
    }
    true
}
