use std::collections::HashMap;
use std::str::FromStr;

use domain::JobDefId;
use plugin_package::{PluginManifest, validate_config_schema};
use store::LibraryReadError;
use store::plugins::{
    AcceptPluginOperation, PluginCommandCause, PluginLifecycle, PluginSubject, PluginTaskDefinition,
};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{OpBegin, load_app_operation_admin_view};
use super::lifecycle_ops::{check_management_replay, load_operation_admin_view};
use super::op_settlement::{
    complete_foreground_non_staged_op, complete_foreground_non_staged_op_with_result,
    complete_non_staged_app_op,
};
use super::reauth::now_unix;
use super::service::{PluginService, run_app_read, run_blocking};
use super::tasks::sync_task_binding_with_handles;
use super::types::{
    OperationAdminView, PagedView, PluginServiceError, RunTaskInput, TaskAdminView,
    TaskReceiptView, TaskScheduleView, UpdateTaskInput, format_rfc3339, map_package_err,
};
use crate::management::ApiState;

impl PluginService {
    pub async fn list_tasks(
        &self,
        state: &ApiState,
        plugin_id: &str,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<PagedView<TaskAdminView>, PluginServiceError> {
        let pid = plugin_id.to_string();
        let page = run_app_read(state, move |store| {
            let _ = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            Ok(store.list_plugin_task_bindings(&pid, cursor.as_deref(), limit)?)
        })
        .await?;

        let def_ids: Vec<JobDefId> = page
            .items
            .iter()
            .filter_map(|b| {
                b.job_def_id
                    .as_deref()
                    .and_then(|s| JobDefId::from_str(s).ok())
            })
            .collect();
        let mut children_by_def: HashMap<JobDefId, jobs::JobChildren> = HashMap::new();
        if !def_ids.is_empty() {
            let rows = crate::jobs_readers::read_async(state.jobs_readers.clone(), move |queue| {
                queue.children_for_defs(&def_ids)
            })
            .await
            .map_err(map_jobs_read_error)?;
            for ch in rows {
                children_by_def.insert(ch.def_id, ch);
            }
        }

        let items = page
            .items
            .into_iter()
            .map(|b| task_admin_view(b, &children_by_def))
            .collect();
        Ok(PagedView {
            items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn update_task(
        &self,
        state: &ApiState,
        plugin_id: &str,
        task_id: &str,
        input: UpdateTaskInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "task-update",
        )
        .await?
        {
            return Ok(replayed);
        }
        if !(60..=86_400).contains(&input.interval_secs) {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                "task interval_secs must be between 60 and 86400",
            ));
        }

        let pid_read = plugin_id.to_string();
        let ver = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid_read)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            store
                .get_plugin_version(&pid_read, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })
        })
        .await?;

        let data_dir = state.store.data_dir().to_path_buf();
        let ver_val = ver.clone();
        let tid_val = task_id.to_string();
        let cfg_val = input.config.clone();
        run_blocking(move || {
            validate_task_declared_and_input_sync(&data_dir, &ver_val, &tid_val, &cfg_val, false)
        })
        .await?;

        let pid = plugin_id.to_string();
        let tid = task_id.to_string();
        let cfg = input.config;
        let expected_rev = input.expected_revision;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key;
        let req = input.request;
        let expected_hash = ver.package_hash;

        let (binding, op_id) = match run_app_write_tx(state, move |app| {
            let p = app.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
                plugin_id: Some(pid.clone()),
                subject: PluginSubject::User(actor),
                idempotency_key: idem,
                request: req,
                kind: "task-update".into(),
                expected_revision: Some(p.revision),
                expected_generation: None,
                cause: PluginCommandCause::root(format!("task-update:{pid}:{tid}")),
            })?;
            let op_id = accepted.operation.operation_id;
            if accepted.replayed {
                return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                    app, op_id,
                )?)));
            }
            if p.package_hash != expected_hash {
                return Err(PluginServiceError::conflict(
                    "plugin.revision_conflict",
                    "plugin package changed",
                ));
            }
            let claim =
                app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
            let binding = app.save_plugin_task_binding(
                &p,
                &tid,
                expected_rev,
                &PluginTaskDefinition {
                    enabled: input.enabled,
                    interval_secs: Some(input.interval_secs),
                    public_config: cfg,
                },
            )?;
            complete_non_staged_app_op(app, &claim, true, "task-updated")?;
            Ok(OpBegin::Proceed((binding, op_id)))
        })
        .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(pair) => pair,
        };

        let s = state.clone();
        run_blocking(move || {
            let _ = sync_task_binding_with_handles(&s.store, &s.jobs, &binding);
            load_operation_admin_view(&s.store, op_id)
        })
        .await
    }

    pub async fn run_task(
        &self,
        state: &ApiState,
        plugin_id: &str,
        task_id: &str,
        input: RunTaskInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        if let Some(replayed) = check_management_replay(
            state,
            input.actor_user_id,
            Some(plugin_id),
            &input.idempotency_key,
            &input.request,
            "task-run",
        )
        .await?
        {
            return Ok(replayed);
        }

        let pid_read = plugin_id.to_string();
        let tid_read = task_id.to_string();
        let (ver, existing_binding) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid_read)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
                return Err(PluginServiceError::conflict(
                    "plugin.not_enabled",
                    "plugin must be enabled to run tasks",
                ));
            }
            let ver = store
                .get_plugin_version(&pid_read, &p.package_hash)?
                .ok_or_else(|| {
                    PluginServiceError::not_found("plugin.missing", "plugin version missing")
                })?;
            let binding = store.get_plugin_task_binding(&pid_read, &tid_read)?;
            Ok((ver, binding))
        })
        .await?;

        let has_override =
            !input.input.is_null() && !input.input.as_object().is_some_and(|m| m.is_empty());
        let effective_input = if has_override {
            input.input.clone()
        } else {
            existing_binding.as_ref().map_or_else(
                || serde_json::json!({}),
                |b| b.definition.public_config.clone(),
            )
        };
        let data_dir = state.store.data_dir().to_path_buf();
        let ver_val = ver.clone();
        let tid_val = task_id.to_string();
        run_blocking(move || {
            validate_task_declared_and_input_sync(
                &data_dir,
                &ver_val,
                &tid_val,
                &effective_input,
                false,
            )
        })
        .await?;

        let run_input = input.input.clone();
        let (binding, claim) =
            match begin_and_claim_run_task(state, plugin_id, task_id, input, ver.package_hash)
                .await?
            {
                OpBegin::Replayed(view) => return Ok(*view),
                OpBegin::Proceed(pair) => pair,
            };

        let s = state.clone();
        run_blocking(move || dispatch_task_run_blocking(&s, &binding, &claim, run_input)).await
    }

    pub async fn list_task_receipts(
        &self,
        state: &ApiState,
        plugin_id: &str,
        task_id: &str,
        cursor: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<PagedView<TaskReceiptView>, PluginServiceError> {
        let pid = plugin_id.to_string();
        let tid = task_id.to_string();
        run_app_read(state, move |store| {
            let _ = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let page = store.list_plugin_task_receipts(&pid, &tid, cursor, limit)?;
            Ok(PagedView {
                items: page
                    .items
                    .into_iter()
                    .map(|r| TaskReceiptView {
                        command_id: r.operation_id,
                        operation_id: r.operation_id,
                        job_id: None,
                        scope: r.targets,
                        generation: r.generation,
                        state: r.state,
                        phase: r.phase,
                        effect: super::admin_ops::effect_label(r.effect),
                        evidence: None,
                        observed_at: format_rfc3339(r.updated_at),
                    })
                    .collect(),
                next_cursor: page.next_cursor,
            })
        })
        .await
    }
}

async fn begin_and_claim_run_task(
    state: &ApiState,
    plugin_id: &str,
    task_id: &str,
    input: RunTaskInput,
    expected_hash: String,
) -> Result<
    OpBegin<(
        store::plugins::PluginTaskBinding,
        store::plugins::PluginCommandClaim,
    )>,
    PluginServiceError,
> {
    let pid = plugin_id.to_string();
    let tid = task_id.to_string();
    let run_input_write = input.input;
    let expected_rev = input.expected_revision;
    let actor = input.actor_user_id;
    let idem = input.idempotency_key;
    let req = input.request;
    run_app_write_tx(state, move |app| {
        let p = app
            .get_plugin(&pid)?
            .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
        let accepted = app.accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.clone()),
            subject: PluginSubject::User(actor),
            idempotency_key: idem,
            request: req,
            kind: "task-run".into(),
            expected_revision: Some(p.revision),
            expected_generation: None,
            cause: PluginCommandCause::root(format!("task-run:{pid}:{tid}")),
        })?;
        let op_id = accepted.operation.operation_id;
        if accepted.replayed {
            return Ok(OpBegin::Replayed(Box::new(load_app_operation_admin_view(
                app, op_id,
            )?)));
        }
        if p.package_hash != expected_hash {
            return Err(PluginServiceError::conflict(
                "plugin.revision_conflict",
                "plugin package changed",
            ));
        }
        if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
            return Err(PluginServiceError::conflict(
                "plugin.not_enabled",
                "plugin must be enabled to run tasks",
            ));
        }
        let binding = match app.get_plugin_task_binding(&pid, &tid)? {
            Some(b) => {
                if b.definition_revision != expected_rev {
                    return Err(PluginServiceError::conflict(
                        "plugin.revision_conflict",
                        "task definition revision mismatch",
                    ));
                }
                if !b.definition.enabled {
                    return Err(PluginServiceError::conflict(
                        "plugin.conflict",
                        "task must be enabled before running",
                    ));
                }
                b
            }
            None => {
                if expected_rev != 0 {
                    return Err(PluginServiceError::conflict(
                        "plugin.revision_conflict",
                        "task definition revision mismatch",
                    ));
                }
                let has_override = !run_input_write.is_null()
                    && !run_input_write.as_object().is_some_and(|m| m.is_empty());
                let saved_cfg = if has_override {
                    run_input_write
                } else {
                    serde_json::json!({})
                };
                app.save_plugin_task_binding(
                    &p,
                    &tid,
                    0,
                    &PluginTaskDefinition {
                        enabled: true,
                        interval_secs: Some(300),
                        public_config: saved_cfg,
                    },
                )?
            }
        };
        let claim =
            app.claim_plugin_command(op_id, accepted.operation.revision, None, 0, now_unix())?;
        Ok(OpBegin::Proceed((binding, claim)))
    })
    .await
}

fn task_admin_view(
    b: store::plugins::PluginTaskBinding,
    children_by_def: &HashMap<JobDefId, jobs::JobChildren>,
) -> TaskAdminView {
    let def_id = b
        .job_def_id
        .as_deref()
        .and_then(|s| JobDefId::from_str(s).ok());
    let children = def_id.and_then(|id| children_by_def.get(&id));
    let next_run_after = if !b.definition.enabled {
        None
    } else {
        children.and_then(|ch| {
            ch.live
                .as_ref()
                .map(|j| format_rfc3339(j.run_after))
                .or_else(|| {
                    ch.last
                        .as_ref()
                        .and_then(|j| j.finished_at)
                        .and_then(|fin| {
                            b.definition
                                .interval_secs
                                .map(|s| format_rfc3339(fin.saturating_add(s)))
                        })
                })
        })
    };
    let last_job = children
        .and_then(|ch| {
            ch.last
                .as_ref()
                .or(ch.live.as_ref())
                .map(|j| j.id.to_string())
        })
        .or(b.job_def_id);
    TaskAdminView {
        id: b.task_id,
        binding_id: b.binding_id,
        plugin_id: b.plugin_id,
        definition_revision: b.definition_revision,
        enabled: b.definition.enabled,
        schedule: TaskScheduleView {
            interval_secs: b.definition.interval_secs,
        },
        config: b.definition.public_config,
        last_job,
        next_run_after,
        ownership_summary: "plugin".into(),
    }
}

fn dispatch_task_run_blocking(
    s: &ApiState,
    binding: &store::plugins::PluginTaskBinding,
    claim: &store::plugins::PluginCommandClaim,
    run_input: serde_json::Value,
) -> Result<OperationAdminView, PluginServiceError> {
    let op_id = claim.operation_id();
    let res = (|| -> Result<Option<domain::JobId>, PluginServiceError> {
        sync_task_binding_with_handles(&s.store, &s.jobs, binding)?;
        let reservation = s.jobs.reserve()?;
        let queue = reservation.acquire()?;
        let bid = domain::PluginTaskBindingId::from_uuid(binding.binding_id);
        let identity = jobs::JobIdentity::plugin_task(bid);
        let created = queue.ensure_scheduled_identity(0, identity)?;
        let target_job = match created.into_iter().next() {
            Some(j) => Some(j),
            None => queue
                .def_for_identity(identity)?
                .and_then(|d| queue.get_live_child(d.id).ok().flatten()),
        };
        let has_override =
            !run_input.is_null() && !run_input.as_object().is_some_and(|m| m.is_empty());
        if let Some(job) = &target_job
            && job.status == jobs::JobStatus::Queued
            && let Ok(mut obj) = serde_json::from_str::<serde_json::Value>(&job.payload)
            && let Some(map) = obj.as_object_mut()
        {
            let changed = if has_override {
                map.insert("input".into(), run_input);
                true
            } else {
                map.remove("input").is_some()
            };
            if changed && let Ok(updated_payload) = serde_json::to_string(&obj) {
                queue.set_queued_job_payload(job.id, &updated_payload)?;
            }
        }
        Ok(target_job.map(|j| j.id))
    })();
    match res {
        Ok(job_id) => {
            let result_bytes = serde_json::to_vec(&serde_json::json!({
                "binding_id": binding.binding_id,
                "job_id": job_id.map(|j| j.to_string()),
            }))
            .map_err(|e| PluginServiceError::Io(e.to_string()))?;
            complete_foreground_non_staged_op_with_result(
                &s,
                claim,
                true,
                "dispatched",
                Some(result_bytes),
            )?;
            load_operation_admin_view(&s.store, op_id)
        }
        Err(err) => {
            if !super::recovery::is_transient_or_admission_err(&err) {
                let _ = complete_foreground_non_staged_op(&s, claim, false, "dispatch-failed");
            }
            Err(err)
        }
    }
}

fn map_jobs_read_error(err: LibraryReadError) -> PluginServiceError {
    match err {
        LibraryReadError::Busy
        | LibraryReadError::Deadline
        | LibraryReadError::Cancelled
        | LibraryReadError::Closed => PluginServiceError::Busy {
            code: "runtime.busy",
            message: "jobs reader busy".into(),
        },
        LibraryReadError::Store(e) => PluginServiceError::Store(e),
        other => PluginServiceError::Io(other.to_string()),
    }
}

fn validate_task_declared_and_input_sync(
    data_dir: &std::path::Path,
    version: &store::plugins::PluginVersionMetadata,
    task_id: &str,
    input: &serde_json::Value,
    allow_null: bool,
) -> Result<(), PluginServiceError> {
    if version.manifest_json == "{}" {
        return Ok(());
    }
    let manifest: PluginManifest = serde_json::from_str(&version.manifest_json)
        .map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let task_decl = manifest
        .components
        .iter()
        .flat_map(|c| c.tasks.iter())
        .find(|t| t.id == task_id)
        .ok_or_else(|| {
            PluginServiceError::not_found(
                "plugin.task_missing",
                format!("task `{task_id}` not declared"),
            )
        })?;
    if allow_null && input.is_null() {
        return Ok(());
    }
    if version.signature_status == "builtin" || version.artifact_path.trim().is_empty() {
        let json_val: serde_json::Value =
            serde_json::from_str(&task_decl.input_schema).map_err(|e| {
                PluginServiceError::invalid(
                    "plugin.invalid_package",
                    format!("task `{task_id}` inline input schema invalid JSON: {e}"),
                )
            })?;
        let schema = validate_config_schema(&json_val).map_err(map_package_err)?;
        schema
            .validate_values(input, false)
            .map_err(|fields| PluginServiceError::config_invalid("invalid task input", fields))?;
        return Ok(());
    }
    let base = std::path::Path::new(&version.artifact_path);
    let root = if base.is_absolute() {
        base.to_path_buf()
    } else {
        data_dir.join(base)
    };
    if root.is_dir() {
        let schema_path = super::staging::safe_rel_path(&root, &task_decl.input_schema)?;
        let raw = std::fs::read(&schema_path).map_err(|e| {
            tracing::warn!(
                task_id,
                schema_path = %schema_path.display(),
                error = %e,
                "task input schema file unreadable"
            );
            PluginServiceError::invalid(
                "plugin.invalid_package",
                format!(
                    "task `{task_id}` input schema `{}` unavailable",
                    task_decl.input_schema
                ),
            )
        })?;
        let json_val: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| {
            PluginServiceError::invalid(
                "plugin.invalid_package",
                format!(
                    "task `{task_id}` input schema `{}` invalid JSON: {e}",
                    task_decl.input_schema
                ),
            )
        })?;
        let schema = validate_config_schema(&json_val).map_err(map_package_err)?;
        schema
            .validate_values(input, false)
            .map_err(|fields| PluginServiceError::config_invalid("invalid task input", fields))?;
    }
    Ok(())
}
