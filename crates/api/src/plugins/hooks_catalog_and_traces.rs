use std::collections::{HashMap, VecDeque};

use domain::SubscribeId;
use hooks::{Gate, HookStepStatus, HookTrace, RescoreBudget, sanitize_reason};
use plugin_api::{Candidate, HOOK_CATALOG};
use serde_json::{Value, json};
use store::plugins::{
    PluginEffectObservation, PluginLifecycle, PluginOperationState, RecordPluginAudit,
};

use crate::management::ApiState;

const MAX_RECENT_TRACES: usize = 64;

#[derive(Default)]
pub(crate) struct HookTraceBuffers {
    checkin: VecDeque<HookTrace>,
    subscribe: HashMap<SubscribeId, VecDeque<HookTrace>>,
}

impl HookTraceBuffers {
    pub(crate) fn record_checkin(&mut self, trace: HookTrace) {
        if self.checkin.len() >= MAX_RECENT_TRACES {
            self.checkin.pop_front();
        }
        self.checkin.push_back(trace);
    }

    pub(crate) fn take_checkin(&mut self) -> Vec<HookTrace> {
        self.checkin.drain(..).collect()
    }

    pub(crate) fn record_subscribe(&mut self, id: SubscribeId, trace: HookTrace) {
        let queue = self.subscribe.entry(id).or_default();
        if queue.len() >= MAX_RECENT_TRACES {
            queue.pop_front();
        }
        queue.push_back(trace);
    }

    pub(crate) fn list_subscribe(&self, id: SubscribeId) -> Vec<HookTrace> {
        self.subscribe
            .get(&id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

pub(crate) fn hook_catalog_items() -> Vec<Value> {
    HOOK_CATALOG
        .iter()
        .map(|entry| {
            let mut val = serde_json::to_value(entry).unwrap_or_else(|_| json!({ "id": entry.id }));
            if let Some(obj) = val.as_object_mut() {
                obj.insert("wired".into(), Value::Bool(true));
                obj.insert(
                    "failure_policy".into(),
                    Value::String(if entry.id == "download.before" {
                        "closed".into()
                    } else {
                        "open".into()
                    }),
                );
                obj.insert(
                    "default_timeout_ms".into(),
                    Value::from(if entry.id == "download.before" {
                        500
                    } else {
                        200
                    }),
                );
            }
            val
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct ApiSubscribeHooks {
    state: ApiState,
}

impl ApiSubscribeHooks {
    pub(crate) fn new(state: ApiState) -> Self {
        Self { state }
    }
}

impl subscribe::SubscribePolicyHooks for ApiSubscribeHooks {
    fn filter_after(
        &self,
        subscribe_id: SubscribeId,
        candidates: &mut Vec<Candidate>,
        admitted: &mut Vec<String>,
        budget: &mut RescoreBudget,
    ) {
        let _ = super::hooks_runtime::run_subscribe_filter_after(
            &self.state,
            subscribe_id,
            candidates,
            admitted,
            budget,
        );
    }

    fn score(
        &self,
        subscribe_id: SubscribeId,
        media_id: domain::MediaId,
        candidates: &mut Vec<Candidate>,
        admitted: &mut Vec<String>,
        budget: &mut RescoreBudget,
    ) {
        let _ = super::hooks_runtime::run_subscribe_score(
            &self.state,
            subscribe_id,
            media_id,
            candidates,
            admitted,
            budget,
        );
    }

    fn select(
        &self,
        subscribe_id: SubscribeId,
        media_id: domain::MediaId,
        candidates: &mut Vec<Candidate>,
        admitted: &mut Vec<String>,
    ) -> Gate {
        let (gate, _) = super::hooks_runtime::run_subscribe_select(
            &self.state,
            subscribe_id,
            media_id,
            candidates,
            admitted,
        );
        gate
    }
}

pub(crate) fn record_hook_trace_audit(
    state: &ApiState,
    hook_id: &str,
    workflow_id: &str,
    trace: &HookTrace,
    boundary_rejected: bool,
) {
    if trace.steps.is_empty() || !state.instance_writable() || state.write_gate.is_closed() {
        return;
    }
    let generations: HashMap<String, i64> =
        super::PluginService::list_all_plugins(&state.store, Some(PluginLifecycle::Enabled))
            .map(|items| {
                items
                    .into_iter()
                    .map(|r| (r.plugin_id, r.generation))
                    .collect()
            })
            .unwrap_or_default();

    for step in &trace.steps {
        let reason = step
            .reason
            .as_deref()
            .map(sanitize_reason)
            .unwrap_or_else(|| sanitize_reason(&step.decision));
        let (op_state, phase) = if boundary_rejected && step.status == HookStepStatus::Applied {
            (
                PluginOperationState::Rejected,
                "rejected: destination outside owning library root".to_string(),
            )
        } else {
            match step.status {
                HookStepStatus::Applied | HookStepStatus::Allowed => {
                    (PluginOperationState::Succeeded, step.decision.clone())
                }
                HookStepStatus::RolledBack => {
                    if reason.starts_with("invalid patch") || reason.contains("unsupported patch") {
                        (
                            PluginOperationState::Rejected,
                            format!("rejected: {reason}"),
                        )
                    } else {
                        (PluginOperationState::Failed, format!("error: {reason}"))
                    }
                }
                HookStepStatus::Rejected | HookStepStatus::Deferred => (
                    PluginOperationState::Rejected,
                    format!("rejected: {reason}"),
                ),
                HookStepStatus::Error => (PluginOperationState::Failed, format!("error: {reason}")),
                HookStepStatus::Skipped if step.decision == "noop" => {
                    (PluginOperationState::Succeeded, step.decision.clone())
                }
                HookStepStatus::Skipped => (PluginOperationState::Cancelled, step.decision.clone()),
            }
        };
        if let Err(err) = state.store.append_plugin_audit(&RecordPluginAudit {
            plugin_id: step.plugin_id.clone(),
            generation: generations.get(&step.plugin_id).copied(),
            action: hook_id.to_string(),
            method: "HOOK".to_string(),
            path: format!("/hooks/{hook_id}"),
            cause_id: workflow_id.to_string(),
            state: op_state,
            phase,
            effect: PluginEffectObservation::NoEffect,
        }) {
            tracing::warn!(
                plugin_id = %step.plugin_id,
                hook_id,
                error = %err,
                "Failed to persist hook audit event"
            );
        }
    }
}
