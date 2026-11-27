use std::time::Instant;

use plugin_api::{ErrorCode, ErrorDetail, Event, Failure, GateInput, UpstreamKind};

use crate::pipeline::{DispatchGuard, Gate, GateFailurePolicy, HookPipeline, Registration};
use crate::trace::{
    HookStepStatus, HookStepTrace, HookTrace, elapsed_ms, push_step, sanitize_reason,
    stamp_step_context,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyErrorDisposition {
    Retryable { retry_after_ms: Option<u64> },
    Cancelled,
    Permanent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyStepOutcome {
    pub plugin_id: String,
    pub component: String,
    pub failure: Option<Failure>,
    pub disposition: Option<NotifyErrorDisposition>,
    pub sanitized_reason: Option<String>,
    pub duration_ms: u64,
}

pub fn classify_notify_failure(failure: &Failure) -> NotifyErrorDisposition {
    match failure {
        Failure::Busy | Failure::DeadlineExceeded => NotifyErrorDisposition::Retryable {
            retry_after_ms: None,
        },
        Failure::Cancelled => NotifyErrorDisposition::Cancelled,
        Failure::Guest(err) => match err.code {
            ErrorCode::Busy | ErrorCode::DeadlineExceeded | ErrorCode::UnknownOutcome => {
                NotifyErrorDisposition::Retryable {
                    retry_after_ms: err.retry_after_ms,
                }
            }
            ErrorCode::Cancelled => NotifyErrorDisposition::Cancelled,
            ErrorCode::Upstream
                if matches!(
                    err.detail,
                    Some(ErrorDetail::Upstream(
                        UpstreamKind::RateLimited | UpstreamKind::Network
                    ))
                ) =>
            {
                NotifyErrorDisposition::Retryable {
                    retry_after_ms: err.retry_after_ms,
                }
            }
            _ => NotifyErrorDisposition::Permanent,
        },
        Failure::Reentrant
        | Failure::Unsupported
        | Failure::Trap(_)
        | Failure::InvalidComponent(_)
        | Failure::InvalidPatch(_)
        | Failure::InvalidInput(_) => NotifyErrorDisposition::Permanent,
    }
}

enum GateStepOutcome {
    Continue,
    Break(Gate, String, Option<String>),
}

fn evaluate_gate_step(
    policy: GateFailurePolicy,
    idx: usize,
    reg: Registration,
    input: &GateInput,
) -> (HookStepTrace, GateStepOutcome) {
    let step_started = Instant::now();
    let mut per_plugin_input = input.clone();
    per_plugin_input.context = stamp_step_context(&input.context, &reg, idx);
    match reg.invoker.gate_typed(&per_plugin_input) {
        Ok(Gate::Allow) => (
            HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Allowed,
                decision: "allow".into(),
                reason: None,
                duration_ms: elapsed_ms(step_started),
            },
            GateStepOutcome::Continue,
        ),
        Ok(Gate::Abstain) => (
            HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Skipped,
                decision: "abstain".into(),
                reason: None,
                duration_ms: elapsed_ms(step_started),
            },
            GateStepOutcome::Continue,
        ),
        Ok(Gate::Reject(reason)) => {
            let clean = sanitize_reason(&reason);
            let step = HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Rejected,
                decision: "reject".into(),
                reason: Some(clean.clone()),
                duration_ms: elapsed_ms(step_started),
            };
            (
                step,
                GateStepOutcome::Break(Gate::Reject(clean.clone()), "reject".into(), Some(clean)),
            )
        }
        Ok(Gate::Defer { until_ms, reason }) => {
            let clean = sanitize_reason(&reason);
            let step = HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Deferred,
                decision: "defer".into(),
                reason: Some(clean.clone()),
                duration_ms: elapsed_ms(step_started),
            };
            (
                step,
                GateStepOutcome::Break(
                    Gate::Defer {
                        until_ms,
                        reason: clean.clone(),
                    },
                    "defer".into(),
                    Some(clean),
                ),
            )
        }
        Err(error) => record_gate_error_step(policy, reg, &input.hook_id, &error, step_started),
    }
}

fn record_gate_error_step(
    policy: GateFailurePolicy,
    reg: Registration,
    hook_id: &str,
    error: &str,
    step_started: Instant,
) -> (HookStepTrace, GateStepOutcome) {
    let clean = sanitize_reason(error);
    tracing::warn!(
        error = %clean,
        plugin = %reg.plugin,
        component = %reg.component,
        hook_id = %hook_id,
        "Typed hook gate failed"
    );
    let closed_defer = hook_id == "download.before";
    let decision = if policy == GateFailurePolicy::Closed {
        if closed_defer { "defer" } else { "reject" }
    } else {
        "error_open"
    };
    let step = HookStepTrace {
        plugin_id: reg.plugin,
        component: reg.component,
        status: HookStepStatus::Error,
        decision: decision.into(),
        reason: Some(clean.clone()),
        duration_ms: elapsed_ms(step_started),
    };
    let outcome = if policy == GateFailurePolicy::Closed {
        if closed_defer {
            GateStepOutcome::Break(
                Gate::Defer {
                    until_ms: 60_000,
                    reason: clean.clone(),
                },
                "defer".into(),
                Some(clean),
            )
        } else {
            GateStepOutcome::Break(Gate::Reject(clean.clone()), "reject".into(), Some(clean))
        }
    } else {
        GateStepOutcome::Continue
    };
    (step, outcome)
}

impl HookPipeline {
    pub fn gate_typed_with_trace(
        &self,
        workflow_id: &str,
        input: &GateInput,
    ) -> Result<(Gate, HookTrace), String> {
        if input.schema_version != 1 {
            return Err("unsupported gate schema_version".into());
        }
        let started_all = Instant::now();
        let _guard = DispatchGuard::enter()?;
        let regs = self.matching_registrations(&input.hook_id)?;
        let mut steps = Vec::new();
        let mut final_gate = Gate::Allow;
        let mut final_decision = "allow".to_string();
        let mut final_reason = None;

        for (idx, reg) in regs.into_iter().enumerate() {
            let (step, outcome) = evaluate_gate_step(self.failure_policy, idx, reg, input);
            push_step(&mut steps, step);
            if let GateStepOutcome::Break(gate, decision, reason) = outcome {
                final_gate = gate;
                final_decision = decision;
                final_reason = reason;
                break;
            }
        }

        let trace = HookTrace {
            id: input.context.invocation_id.clone(),
            hook_id: input.hook_id.clone(),
            workflow_id: workflow_id.to_string(),
            input_revision: input.revision,
            steps,
            decision: final_decision,
            reason: final_reason,
            duration_ms: elapsed_ms(started_all),
        };
        Ok((final_gate, trace))
    }

    pub fn notify_typed_with_trace(&self, workflow_id: &str, event: &Event) -> HookTrace {
        self.notify_typed_outcomes_with_trace(workflow_id, event).0
    }

    pub fn notify_typed_outcomes_with_trace(
        &self,
        workflow_id: &str,
        event: &Event,
    ) -> (HookTrace, Vec<NotifyStepOutcome>) {
        let started_all = Instant::now();
        if event.schema_version != 1 {
            let reason = "unsupported event schema_version";
            return notify_host_error_result(
                event,
                workflow_id,
                reason,
                Failure::InvalidInput(reason.into()),
                started_all,
            );
        }
        let Ok(_guard) = DispatchGuard::enter() else {
            let reason = "recursive Hook dispatch";
            return notify_host_error_result(
                event,
                workflow_id,
                reason,
                Failure::Reentrant,
                started_all,
            );
        };
        let regs = match self.matching_registrations(&event.kind) {
            Ok(regs) => regs,
            Err(err) => {
                let clean = sanitize_reason(&err);
                return (
                    HookTrace {
                        id: event.context.invocation_id.clone(),
                        hook_id: event.kind.clone(),
                        workflow_id: workflow_id.to_string(),
                        input_revision: event.revision,
                        steps: vec![],
                        decision: "notify_error".into(),
                        reason: Some(clean.clone()),
                        duration_ms: elapsed_ms(started_all),
                    },
                    vec![NotifyStepOutcome {
                        plugin_id: "host".into(),
                        component: "notify".into(),
                        failure: Some(Failure::Trap(err)),
                        disposition: Some(NotifyErrorDisposition::Permanent),
                        sanitized_reason: Some(clean),
                        duration_ms: elapsed_ms(started_all),
                    }],
                );
            }
        };
        let mut steps = Vec::new();
        let mut outcomes = Vec::with_capacity(regs.len());
        let mut had_error = false;
        for (idx, reg) in regs.into_iter().enumerate() {
            let (step, outcome, errored) = run_notify_step(idx, reg, event);
            had_error |= errored;
            outcomes.push(outcome);
            push_step(&mut steps, step);
        }
        let trace = HookTrace {
            id: event.context.invocation_id.clone(),
            hook_id: event.kind.clone(),
            workflow_id: workflow_id.to_string(),
            input_revision: event.revision,
            steps,
            decision: if had_error {
                "notified_with_errors".into()
            } else {
                "notified".into()
            },
            reason: None,
            duration_ms: elapsed_ms(started_all),
        };
        (trace, outcomes)
    }
}

fn run_notify_step(
    idx: usize,
    reg: Registration,
    event: &Event,
) -> (HookStepTrace, NotifyStepOutcome, bool) {
    let step_started = Instant::now();
    let mut per_plugin_event = event.clone();
    per_plugin_event.context = stamp_step_context(&event.context, &reg, idx);
    match reg.invoker.notify_typed_result(&per_plugin_event) {
        Ok(()) => {
            let duration_ms = elapsed_ms(step_started);
            let outcome = NotifyStepOutcome {
                plugin_id: reg.plugin.clone(),
                component: reg.component.clone(),
                failure: None,
                disposition: None,
                sanitized_reason: None,
                duration_ms,
            };
            let step = HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Applied,
                decision: "notified".into(),
                reason: None,
                duration_ms,
            };
            (step, outcome, false)
        }
        Err(failure) => {
            let duration_ms = elapsed_ms(step_started);
            let disposition = classify_notify_failure(&failure);
            let clean = sanitize_reason(&failure.to_string());
            tracing::warn!(
                error = %clean,
                plugin = %reg.plugin,
                component = %reg.component,
                event_kind = %event.kind,
                ?disposition,
                "Typed hook notify failed"
            );
            let outcome = NotifyStepOutcome {
                plugin_id: reg.plugin.clone(),
                component: reg.component.clone(),
                failure: Some(failure),
                disposition: Some(disposition),
                sanitized_reason: Some(clean.clone()),
                duration_ms,
            };
            let step = HookStepTrace {
                plugin_id: reg.plugin,
                component: reg.component,
                status: HookStepStatus::Error,
                decision: "error".into(),
                reason: Some(clean),
                duration_ms,
            };
            (step, outcome, true)
        }
    }
}

fn notify_host_error_result(
    event: &Event,
    workflow_id: &str,
    reason: &str,
    failure: Failure,
    started_all: Instant,
) -> (HookTrace, Vec<NotifyStepOutcome>) {
    let duration_ms = elapsed_ms(started_all);
    let disposition = classify_notify_failure(&failure);
    let trace = HookTrace {
        id: event.context.invocation_id.clone(),
        hook_id: event.kind.clone(),
        workflow_id: workflow_id.to_string(),
        input_revision: event.revision,
        steps: vec![HookStepTrace {
            plugin_id: "host".into(),
            component: "notify".into(),
            status: HookStepStatus::Error,
            decision: "error".into(),
            reason: Some(reason.into()),
            duration_ms: 0,
        }],
        decision: "notify_error".into(),
        reason: Some(reason.into()),
        duration_ms,
    };
    let outcomes = vec![NotifyStepOutcome {
        plugin_id: "host".into(),
        component: "notify".into(),
        failure: Some(failure),
        disposition: Some(disposition),
        sanitized_reason: Some(reason.into()),
        duration_ms,
    }];
    (trace, outcomes)
}
