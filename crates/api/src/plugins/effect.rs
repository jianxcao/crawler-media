use std::str::FromStr;
use std::sync::Arc;

use domain::{JobId, PluginOperationId};
use plugin_package::PluginManifest;
use serde::Deserialize;
use store::Store;
use store::plugins::{
    PluginCommandCause, PluginGrantScope, PluginLifecycle, PluginReconciliationOutcome,
    PluginRecord, PluginStateAuthority, PluginSubject, PluginTaskBinding, PluginTaskDefinition,
};

use super::tasks::sync_task_binding_with_handles;
use crate::JobsWriter;
use crate::management::ApiState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginInvocationRole {
    Filter,
    Gate,
    Notify,
    Task,
    Endpoint,
    ReadOnlyEndpoint,
    Indexer,
    Maintenance,
    Lifecycle,
    Management,
}

impl PluginInvocationRole {
    pub fn allows_command_submit(self) -> bool {
        matches!(self, Self::Task | Self::Endpoint | Self::Management)
    }

    pub fn from_guest_role(role: plugin_api::GuestRole) -> Self {
        match role {
            plugin_api::GuestRole::Filter => Self::Filter,
            plugin_api::GuestRole::Gate => Self::Gate,
            plugin_api::GuestRole::Notify => Self::Notify,
            plugin_api::GuestRole::Task => Self::Task,
            plugin_api::GuestRole::Endpoint => Self::Endpoint,
            plugin_api::GuestRole::Indexer => Self::Indexer,
            plugin_api::GuestRole::Maintenance => Self::Maintenance,
            plugin_api::GuestRole::Lifecycle => Self::Lifecycle,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalEffectStep {
    Succeeded { result: Option<Vec<u8>> },
    Deferred { next_run_at: i64, phase: String },
    Rejected { phase: String },
    Failed { phase: String },
    Unknown { phase: String },
}

pub(crate) fn sanitize_phase_token(raw: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(128));
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || "-_:./".contains(ch) {
            if out.len() >= 128 {
                break;
            }
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            if out.len() >= 128 {
                break;
            }
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    let mut fb = String::with_capacity(fallback.len().min(128));
    for ch in fallback.chars() {
        if (ch.is_ascii_alphanumeric() || "-_:./".contains(ch)) && fb.len() < 128 {
            fb.push(ch);
        }
    }
    if fb.is_empty() { "unknown".into() } else { fb }
}

pub(crate) fn sanitize_prefixed_phase(prefix: &str, detail: impl std::fmt::Display) -> String {
    let p = sanitize_phase_token(prefix, "error");
    let d = sanitize_phase_token(&detail.to_string(), "unknown");
    let full = format!("{p}:{d}");
    if full.len() <= 128 {
        return full;
    }
    let cut: String = full.chars().take(128).collect();
    let trimmed = cut.trim_end_matches('-');
    if trimmed.is_empty() {
        p
    } else {
        trimmed.to_string()
    }
}

pub struct PluginEffectContext<'a> {
    pub operation_id: PluginOperationId,
    pub plugin_id: Option<&'a str>,
    pub kind: &'a str,
    pub attempt: i64,
    pub generation: Option<i64>,
    pub subject: &'a PluginSubject,
    pub canonical_method: &'a str,
    pub targets: &'a [PluginGrantScope],
    pub cause: &'a PluginCommandCause,
    pub payload: Option<&'a [u8]>,
    pub now: i64,
}

pub trait PluginEffectAdapter: Send + Sync {
    fn supports_kind(&self, kind: &str) -> bool;
    fn execute(&self, ctx: &PluginEffectContext<'_>) -> ExternalEffectStep;
    fn reconcile(
        &self,
        operation_id: PluginOperationId,
        kind: &str,
        attempt: i64,
    ) -> Option<PluginReconciliationOutcome>;
}

#[derive(Default)]
pub(crate) struct ProductionEffectAdapter {
    handles: Option<(Arc<Store>, Arc<JobsWriter>)>,
}

impl ProductionEffectAdapter {
    pub(crate) fn with_handles(store: Arc<Store>, jobs: Arc<JobsWriter>) -> Self {
        Self {
            handles: Some((store, jobs)),
        }
    }
}

#[derive(Deserialize)]
struct ScheduleSetPayload {
    task_id: String,
    interval_secs: i64,
    enabled: bool,
    expected_revision: Option<i64>,
}

#[derive(Deserialize)]
struct ScheduleDisablePayload {
    handle: String,
    revision: i64,
}

fn manifest_declares_task(manifest_json: &str, task_id: &str) -> bool {
    if manifest_json == "{}" {
        return true;
    }
    let Ok(manifest) = serde_json::from_str::<PluginManifest>(manifest_json) else {
        return false;
    };
    manifest
        .components
        .iter()
        .any(|c| c.tasks.iter().any(|t| t.id == task_id))
}

fn execute_schedule_set(
    store: &Store,
    jobs: &JobsWriter,
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
    let Ok(req) = serde_json::from_slice::<ScheduleSetPayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    if req.task_id.trim().is_empty() || !(60..=86_400).contains(&req.interval_secs) {
        return ExternalEffectStep::Rejected {
            phase: "invalid-schedule".into(),
        };
    }
    let Ok(Some(plugin)) = store.get_plugin(plugin_id) else {
        return ExternalEffectStep::Rejected {
            phase: "plugin-missing".into(),
        };
    };
    if plugin.lifecycle != PluginLifecycle::Enabled
        || !plugin.desired_enabled
        || ctx.generation.is_some_and(|g| g != plugin.generation)
    {
        return ExternalEffectStep::Rejected {
            phase: "authority-rejected".into(),
        };
    }
    let Ok(Some(version)) = store.get_plugin_version(plugin_id, &plugin.package_hash) else {
        return ExternalEffectStep::Rejected {
            phase: "version-missing".into(),
        };
    };
    if !manifest_declares_task(&version.manifest_json, &req.task_id) {
        return ExternalEffectStep::Rejected {
            phase: "unknown-task".into(),
        };
    }
    let existing = match store.get_plugin_task_binding(plugin_id, &req.task_id) {
        Ok(b) => b,
        Err(_) => {
            return ExternalEffectStep::Failed {
                phase: "read-binding-failed".into(),
            };
        }
    };
    let expected_rev = req
        .expected_revision
        .unwrap_or_else(|| existing.as_ref().map_or(0, |b| b.definition_revision));
    let public_config = existing
        .as_ref()
        .map(|b| b.definition.public_config.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    let def = PluginTaskDefinition {
        enabled: req.enabled,
        interval_secs: Some(req.interval_secs),
        public_config,
    };
    let saved = match store.save_plugin_task_binding(&plugin, &req.task_id, expected_rev, &def) {
        Ok(b) => b,
        Err(_) => {
            return ExternalEffectStep::Rejected {
                phase: "binding-conflict".into(),
            };
        }
    };
    if let Err(err) = sync_task_binding_with_handles(store, jobs, &saved) {
        tracing::warn!(%err, binding_id = %saved.binding_id, "task sync deferred to reconciliation");
    }
    let receipt = serde_json::to_vec(&serde_json::json!({
        "binding_id": saved.binding_id,
        "task_id": saved.task_id,
        "definition_revision": saved.definition_revision,
        "enabled": saved.definition.enabled,
        "interval_secs": saved.definition.interval_secs,
    }))
    .ok();
    ExternalEffectStep::Succeeded { result: receipt }
}

fn execute_schedule_disable(
    store: &Store,
    jobs: &JobsWriter,
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
    let Ok(req) = serde_json::from_slice::<ScheduleDisablePayload>(raw) else {
        return ExternalEffectStep::Rejected {
            phase: "invalid-payload".into(),
        };
    };
    let Ok(Some(plugin)) = store.get_plugin(plugin_id) else {
        return ExternalEffectStep::Rejected {
            phase: "plugin-missing".into(),
        };
    };
    if plugin.lifecycle != PluginLifecycle::Enabled
        || !plugin.desired_enabled
        || ctx.generation.is_some_and(|g| g != plugin.generation)
    {
        return ExternalEffectStep::Rejected {
            phase: "authority-rejected".into(),
        };
    }
    let binding = if let Ok(uuid) = uuid::Uuid::from_str(&req.handle) {
        store.get_plugin_task_binding_by_id(uuid).ok().flatten()
    } else {
        store
            .get_plugin_task_binding(plugin_id, &req.handle)
            .ok()
            .flatten()
    };
    let Some(binding) = binding.filter(|b| b.plugin_id == plugin_id) else {
        return ExternalEffectStep::Rejected {
            phase: "unknown-task".into(),
        };
    };
    let def = PluginTaskDefinition {
        enabled: false,
        interval_secs: binding.definition.interval_secs,
        public_config: binding.definition.public_config.clone(),
    };
    let saved = match store.save_plugin_task_binding(&plugin, &binding.task_id, req.revision, &def)
    {
        Ok(b) => b,
        Err(_) => {
            return ExternalEffectStep::Rejected {
                phase: "binding-conflict".into(),
            };
        }
    };
    if let Err(err) = sync_task_binding_with_handles(store, jobs, &saved) {
        tracing::warn!(%err, binding_id = %saved.binding_id, "task disable sync deferred to reconciliation");
    }
    let receipt = serde_json::to_vec(&serde_json::json!({
        "binding_id": saved.binding_id,
        "task_id": saved.task_id,
        "definition_revision": saved.definition_revision,
        "enabled": false,
    }))
    .ok();
    ExternalEffectStep::Succeeded { result: receipt }
}

impl PluginEffectAdapter for ProductionEffectAdapter {
    fn supports_kind(&self, kind: &str) -> bool {
        matches!(
            kind,
            "schedule.set"
                | "schedule.disable"
                | "indexer.search"
                | "site.maintenance"
                | "site.check-in"
                | "site.verify-session"
                | "downloader.add"
                | "downloader.control"
                | "downloader.delete-task"
                | "downloader.prepare-cleanup"
                | "downloader.delete-files"
                | "boost.run"
        )
    }

    fn execute(&self, ctx: &PluginEffectContext<'_>) -> ExternalEffectStep {
        let Some((store, jobs)) = &self.handles else {
            return ExternalEffectStep::Rejected {
                phase: "unsupported-command".into(),
            };
        };
        match ctx.kind {
            "schedule.set" => execute_schedule_set(store, jobs, ctx),
            "schedule.disable" => execute_schedule_disable(store, jobs, ctx),
            "indexer.search" => ExternalEffectStep::Rejected {
                phase: "delegate-indexer-search".into(),
            },
            "site.maintenance" | "site.check-in" | "site.verify-session" => {
                ExternalEffectStep::Rejected {
                    phase: "delegate-site-maintenance".into(),
                }
            }
            "downloader.add" => ExternalEffectStep::Rejected {
                phase: "delegate-downloader-add".into(),
            },
            "downloader.control"
            | "downloader.delete-task"
            | "downloader.prepare-cleanup"
            | "downloader.delete-files" => ExternalEffectStep::Rejected {
                phase: "delegate-downloader-command".into(),
            },
            "boost.run" => ExternalEffectStep::Rejected {
                phase: "delegate-boost-run".into(),
            },
            _ => ExternalEffectStep::Rejected {
                phase: "unsupported-command".into(),
            },
        }
    }

    fn reconcile(
        &self,
        _operation_id: PluginOperationId,
        _kind: &str,
        _attempt: i64,
    ) -> Option<PluginReconciliationOutcome> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct PluginTaskInvocation {
    pub binding: PluginTaskBinding,
    pub plugin: PluginRecord,
    pub authority: PluginStateAuthority,
    pub site: Option<store::SiteObservation>,
    pub job_id: JobId,
    pub attempt: u32,
    pub cause: PluginCommandCause,
    pub input_override: Option<serde_json::Value>,
}

pub trait PluginTaskRunner: Send + Sync {
    fn run_task(&self, state: &ApiState, invocation: &PluginTaskInvocation) -> Result<(), String>;
}
