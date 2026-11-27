use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hooks::{
    HookStepStatus, HookStepTrace, NotifyErrorDisposition, PluginInvoker, classify_notify_failure,
    sanitize_reason,
};
use plugin_api::{ApiError, ErrorCode, Event, Failure};
use store::Store;
use store::plugin_events::{ClaimedOutboxDelivery, OutboxDb};
use store::plugins::{PluginError, PluginRecord};

use super::super::events::{EVENT_DELIVERY_LEASE_MS, EventDispatchContext};

pub(super) type DeliveryTarget = (i64, String, Arc<dyn PluginInvoker>);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum ComponentCheckpoint {
    Simple(String),
    PermanentFailed {
        status: String,
        #[serde(default)]
        error_code: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct DeliveryComponentProgress {
    package_hash: String,
    #[serde(default)]
    done: BTreeMap<String, ComponentCheckpoint>,
}

pub(super) struct MultiTargetOutcome {
    pub(super) steps: Vec<HookStepTrace>,
    pub(super) retryable_failure: Option<(Failure, String, Option<u64>)>,
    pub(super) permanent_failure: Option<(Failure, String)>,
    pub(super) claim_lost: bool,
}

fn encode_permanent_failure(failure: &Failure) -> String {
    match failure {
        Failure::Guest(api) => match api.code {
            ErrorCode::WrongContext => "wrong_context".into(),
            ErrorCode::PermissionDenied => "permission_denied".into(),
            ErrorCode::InvalidArgument => "invalid_argument".into(),
            ErrorCode::Upstream => "upstream".into(),
            _ => "guest_other".into(),
        },
        _ => "host_permanent".into(),
    }
}

fn decode_permanent_failure(code: Option<&str>, reason: String) -> Failure {
    let guest_code = match code {
        Some("wrong_context") => Some(ErrorCode::WrongContext),
        Some("permission_denied") => Some(ErrorCode::PermissionDenied),
        Some("invalid_argument") => Some(ErrorCode::InvalidArgument),
        Some("upstream" | "guest_other") => Some(ErrorCode::Upstream),
        _ => None,
    };
    if let Some(ec) = guest_code {
        Failure::Guest(ApiError {
            code: ec,
            message: reason,
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        })
    } else {
        Failure::Trap(reason)
    }
}

fn load_component_progress(raw: &str, package_hash: &str) -> DeliveryComponentProgress {
    if let Ok(parsed) = serde_json::from_str::<DeliveryComponentProgress>(raw)
        && parsed.package_hash == package_hash
    {
        return parsed;
    }
    DeliveryComponentProgress {
        package_hash: package_hash.to_string(),
        done: BTreeMap::new(),
    }
}

fn checkpoint_component_progress(
    store: &Store,
    db: OutboxDb,
    event_id: &str,
    plugin_id: &str,
    claim_nonce: &str,
    progress: &DeliveryComponentProgress,
    now_ms: i64,
) -> bool {
    let Ok(json) = serde_json::to_string(progress) else {
        return true;
    };
    match store.save_delivery_component_state(db, event_id, plugin_id, claim_nonce, &json, now_ms) {
        Ok(_) => true,
        Err(store::StoreError::Plugin(PluginError::Conflict | PluginError::NotFound)) => false,
        Err(err) => {
            tracing::warn!(
                event_id,
                plugin_id,
                error = %err,
                "Failed to save delivery component checkpoint"
            );
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_target_components(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    plugin: &PluginRecord,
    guest_event: &Event,
    targets: Vec<DeliveryTarget>,
    batch_started: Instant,
    clamped_now: i64,
    renew_before_first: bool,
) -> MultiTargetOutcome {
    let mut progress =
        load_component_progress(&item.delivery.component_state_json, &plugin.package_hash);
    let mut out = MultiTargetOutcome {
        steps: Vec::with_capacity(targets.len()),
        retryable_failure: None,
        permanent_failure: None,
        claim_lost: false,
    };
    let mut ran_any = false;

    for (idx, (_priority, comp_id, invoker)) in targets.into_iter().enumerate() {
        match progress.done.get(&comp_id) {
            Some(ComponentCheckpoint::Simple(s)) if s == "succeeded" => continue,
            Some(ComponentCheckpoint::PermanentFailed { status, .. }) if status == "succeeded" => {
                continue;
            }
            Some(ComponentCheckpoint::Simple(s)) if s == "permanent_failed" => {
                out.permanent_failure.get_or_insert_with(|| {
                    let msg = format!("component `{comp_id}` failed permanently");
                    (Failure::Trap(msg.clone()), msg)
                });
                continue;
            }
            Some(ComponentCheckpoint::PermanentFailed {
                status,
                error_code,
                reason,
            }) if status == "permanent_failed" => {
                out.permanent_failure.get_or_insert_with(|| {
                    let msg = reason
                        .clone()
                        .unwrap_or_else(|| format!("component `{comp_id}` failed permanently"));
                    (
                        decode_permanent_failure(error_code.as_deref(), msg.clone()),
                        msg,
                    )
                });
                continue;
            }
            _ => {}
        }
        let elapsed = batch_started.elapsed();
        let step_now = clamped_now.saturating_add(i64::try_from(elapsed.as_millis()).unwrap_or(0));
        if (ran_any || renew_before_first || elapsed >= Duration::from_millis(500))
            && matches!(
                ctx.store.renew_delivery_lease(
                    db,
                    &item.event.event_id,
                    &item.delivery.plugin_id,
                    &item.claim_nonce,
                    step_now,
                    EVENT_DELIVERY_LEASE_MS,
                ),
                Err(store::StoreError::Plugin(
                    PluginError::Conflict | PluginError::NotFound
                ))
            )
        {
            out.claim_lost = true;
            return out;
        }
        ran_any = true;
        if !run_one_target_component(
            ctx,
            db,
            item,
            guest_event,
            idx,
            comp_id,
            &invoker,
            batch_started,
            clamped_now,
            &mut progress,
            &mut out,
        ) {
            out.claim_lost = true;
            return out;
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn run_one_target_component(
    ctx: &EventDispatchContext,
    db: OutboxDb,
    item: &ClaimedOutboxDelivery,
    guest_event: &Event,
    idx: usize,
    comp_id: String,
    invoker: &Arc<dyn PluginInvoker>,
    batch_started: Instant,
    clamped_now: i64,
    progress: &mut DeliveryComponentProgress,
    out: &mut MultiTargetOutcome,
) -> bool {
    let plugin_id = &item.delivery.plugin_id;
    let event_id = &item.event.event_id;
    let claim_nonce = &item.claim_nonce;
    let mut step_event = guest_event.clone();
    if idx > 0 {
        step_event.context.invocation_id = format!("{}-{idx}", guest_event.context.invocation_id);
    }
    let step_started = Instant::now();
    let res = invoker.notify_typed_result(&step_event);
    let step_ms = u64::try_from(step_started.elapsed().as_millis()).unwrap_or(0);
    let done_now =
        clamped_now.saturating_add(i64::try_from(batch_started.elapsed().as_millis()).unwrap_or(0));

    match res {
        Ok(()) => {
            progress.done.insert(
                comp_id.clone(),
                ComponentCheckpoint::Simple("succeeded".into()),
            );
            if !checkpoint_component_progress(
                &ctx.store,
                db,
                event_id,
                plugin_id,
                claim_nonce,
                progress,
                done_now,
            ) {
                return false;
            }
            out.steps.push(HookStepTrace {
                plugin_id: plugin_id.clone(),
                component: comp_id,
                status: HookStepStatus::Applied,
                decision: "notified".to_string(),
                reason: None,
                duration_ms: step_ms,
            });
        }
        Err(failure) => {
            let reason = sanitize_reason(&failure.to_string());
            out.steps.push(HookStepTrace {
                plugin_id: plugin_id.clone(),
                component: comp_id.clone(),
                status: HookStepStatus::Error,
                decision: "error".to_string(),
                reason: Some(reason.clone()),
                duration_ms: step_ms,
            });
            match classify_notify_failure(&failure) {
                NotifyErrorDisposition::Permanent => {
                    progress.done.insert(
                        comp_id,
                        ComponentCheckpoint::PermanentFailed {
                            status: "permanent_failed".into(),
                            error_code: Some(encode_permanent_failure(&failure)),
                            reason: Some(reason.clone()),
                        },
                    );
                    if !checkpoint_component_progress(
                        &ctx.store,
                        db,
                        event_id,
                        plugin_id,
                        claim_nonce,
                        progress,
                        done_now,
                    ) {
                        return false;
                    }
                    out.permanent_failure.get_or_insert((failure, reason));
                }
                NotifyErrorDisposition::Retryable { retry_after_ms } => {
                    match &mut out.retryable_failure {
                        None => out.retryable_failure = Some((failure, reason, retry_after_ms)),
                        Some((_, _, prev_ra)) => *prev_ra = (*prev_ra).max(retry_after_ms),
                    }
                }
                NotifyErrorDisposition::Cancelled => {
                    out.retryable_failure.get_or_insert((failure, reason, None));
                }
            }
        }
    }
    true
}
