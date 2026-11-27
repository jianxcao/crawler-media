use store::plugins::{
    AcceptPluginOperation, AcceptedPluginOperation, PluginCommandCause, PluginEffectObservation,
    PluginError, PluginLifecycle, PluginOperationState, PluginRecord, PluginSubject,
};
use uuid::Uuid;

use super::admin_command_handles::{
    infer_admin_command_targets, normalize_admin_command_kind, resolve_ui_command_handles,
};
use super::commands::project_command_job;
use super::config_ops::run_app_write_tx;
use super::idempotency::{
    OpBegin, ResumeIdempotencyCtx, check_idempotent_replay, check_idempotent_replay_after_write,
    load_app_operation_admin_view, save_resume_claim_meta_in_app, validate_idempotency_input,
};
use super::lifecycle_ops::{
    check_management_replay, check_management_replay_after_write, load_operation_admin_view,
};
use super::reauth::now_unix;
use super::recovery::recover_claimed_staged_operation;
pub(super) use super::service::require_foreground_write_admitted;
use super::service::{PluginService, run_app_read, run_app_write, run_blocking};
use super::types::{
    AuditEventView, CancelOperationInput, OperationAdminView, PagedView, PluginMetricsView,
    PluginServiceError, ResumeOperationInput, SubmitAdminCommandInput, format_rfc3339,
};
use crate::management::ApiState;

impl PluginService {
    pub async fn get_operation(
        &self,
        state: &ApiState,
        op_id: Uuid,
    ) -> Result<OperationAdminView, PluginServiceError> {
        run_app_read(state, move |store| load_operation_admin_view(store, op_id)).await
    }

    pub async fn cancel_operation(
        &self,
        state: &ApiState,
        op_id: Uuid,
        input: CancelOperationInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        require_foreground_write_admitted(state)?;
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_idempotent_replay::<OperationAdminView>(
            state,
            input.actor_user_id,
            "operation-cancel",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }

        let rev = input.expected_revision;
        let actor = input.actor_user_id;
        let idem = input.idempotency_key;
        let req = input.request;
        run_app_write(state, move |store| {
            Ok(store.app_write(|app| {
                if let Some(val) = app.get_plugin_idempotency_envelope(
                    &PluginSubject::User(actor),
                    "operation-cancel",
                    &idem,
                    &req,
                )? {
                    return serde_json::from_value::<OperationAdminView>(val)
                        .map_err(|e| store::StoreError::Protected(e.to_string()));
                }
                let _ = app.cancel_plugin_operation_admin(op_id, rev, now_unix())?;
                super::idempotency::clear_resume_claim_meta_in_app(app, op_id)?;
                let view = load_app_operation_admin_view(app, op_id)
                    .map_err(|e| store::StoreError::Protected(e.to_string()))?;
                let json = serde_json::to_value(&view)?;
                app.put_plugin_idempotency_envelope(
                    &PluginSubject::User(actor),
                    "operation-cancel",
                    &idem,
                    &req,
                    &json,
                )?;
                Ok(view)
            })?)
        })
        .await
    }

    pub async fn resume_operation(
        &self,
        state: &ApiState,
        op_id: Uuid,
        input: ResumeOperationInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        require_foreground_write_admitted(state)?;
        validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = check_idempotent_replay::<OperationAdminView>(
            state,
            input.actor_user_id,
            "operation-resume",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(replayed);
        }
        let in_flight_guard = match self
            .acquire_resume_in_flight(
                state,
                input.actor_user_id,
                &input.idempotency_key,
                &input.request,
            )
            .await?
        {
            OpBegin::Replayed(view) => return Ok(*view),
            OpBegin::Proceed(guard) => guard,
        };
        let op_exec_guard = self.try_acquire_staged_op(op_id).ok_or_else(|| {
            PluginServiceError::conflict("plugin.conflict", "staged operation is already running")
        })?;

        let actor = input.actor_user_id;
        let rev = input.expected_revision;
        let idem_tx = input.idempotency_key.clone();
        let req_tx = input.request.clone();
        let phase1 = match run_app_write_tx(state, move |app| {
            begin_resume_in_app(app, op_id, actor, rev, &idem_tx, &req_tx)
        })
        .await
        {
            Ok(p1) => p1,
            Err(err) => {
                if is_replayable_resume_conflict(&err)
                    && let Some(replayed) =
                        check_idempotent_replay_after_write::<OperationAdminView>(
                            state,
                            actor,
                            "operation-resume",
                            &input.idempotency_key,
                            &input.request,
                        )
                        .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        match phase1 {
            ResumePhase1::Replayed(view) => Ok(*view),
            ResumePhase1::NonStaged { view, updated } => {
                drop(op_exec_guard);
                project_resumed_non_staged_job(state, updated).await?;
                drop(in_flight_guard);
                Ok(view)
            }
            ResumePhase1::Staged {
                kind,
                claim,
                intent,
                now,
            } => {
                let res = finish_staged_resume(state, op_id, kind, claim, intent, now, input).await;
                drop(op_exec_guard);
                drop(in_flight_guard);
                res
            }
        }
    }

    pub async fn submit_admin_command(
        &self,
        state: &ApiState,
        plugin_id: &str,
        input: SubmitAdminCommandInput,
    ) -> Result<OperationAdminView, PluginServiceError> {
        require_foreground_write_admitted(state)?;
        let pid = plugin_id.to_string();
        let actor = input.actor_user_id;
        let idem = input.idempotency_key.clone();
        let req = input.request.clone();
        let kind = input.kind.clone();
        if let Some(replayed) =
            check_management_replay(state, actor, Some(&pid), &idem, &req, &kind).await?
        {
            return Ok(replayed);
        }

        let s = state.clone();
        let work_input = SubmitAdminCommandInput {
            actor_user_id: actor,
            kind,
            payload: input.payload,
            targets: input.targets,
            evidence_ref: input.evidence_ref,
            expected_revision: input.expected_revision,
            idempotency_key: idem,
            request: req,
        };
        let accepted = match run_app_write(state, move |store| {
            accept_admin_command_in_store(&s, store, &pid, work_input)
        })
        .await
        {
            Ok(acc) => acc,
            Err(err) => {
                if is_replayable_admin_cmd_conflict(&err)
                    && let Some(replayed) = check_management_replay_after_write(
                        state,
                        actor,
                        Some(plugin_id),
                        &input.idempotency_key,
                        &input.request,
                        &input.kind,
                    )
                    .await?
                {
                    return Ok(replayed);
                }
                return Err(err);
            }
        };

        let s2 = state.clone();
        run_blocking(move || project_and_load_admin_command(&s2, accepted)).await
    }

    pub async fn list_audit(
        &self,
        state: &ApiState,
        plugin_id: &str,
        action: Option<String>,
        from_ts: Option<i64>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<PagedView<AuditEventView>, PluginServiceError> {
        let pid = plugin_id.to_string();
        run_app_read(state, move |store| {
            let plugin_exists = store.get_plugin(&pid)?.is_some();
            let page = store.list_plugin_audit_events(
                &pid,
                action.as_deref(),
                from_ts,
                cursor.as_deref(),
                limit,
            )?;
            if !plugin_exists
                && page.items.is_empty()
                && (action.is_none() && from_ts.is_none() && cursor.is_none()
                    || store
                        .list_plugin_audit_events(&pid, None, None, None, Some(1))?
                        .items
                        .is_empty())
            {
                return Err(PluginServiceError::not_found(
                    "plugin.missing",
                    "plugin not found",
                ));
            }
            Ok(PagedView {
                items: page
                    .items
                    .into_iter()
                    .map(|e| AuditEventView {
                        operation_id: e.operation_id,
                        plugin_id: e.plugin_id,
                        action: e.action,
                        subject_kind: e.subject_kind,
                        canonical_method: e.canonical_method,
                        canonical_path: e.canonical_path,
                        state: e.state,
                        phase: e.phase,
                        effect: effect_label(e.effect),
                        created_at: format_rfc3339(e.created_at),
                        updated_at: format_rfc3339(e.updated_at),
                    })
                    .collect(),
                next_cursor: page.next_cursor,
            })
        })
        .await
    }

    pub async fn get_metrics(
        &self,
        state: &ApiState,
        plugin_id: &str,
        window_secs: Option<u64>,
    ) -> Result<PluginMetricsView, PluginServiceError> {
        let pid = plugin_id.to_string();
        let (plugin, unknown_ops) = run_app_read(state, move |store| {
            let p = store.get_plugin(&pid)?.ok_or_else(|| {
                PluginServiceError::not_found("plugin.missing", "plugin not found")
            })?;
            let unknown_ops = store
                .unresolved_plugin_commands(None, Some(200))?
                .into_iter()
                .filter(|u| {
                    u.operation.plugin_id.as_deref() == Some(pid.as_str())
                        && u.operation.state == PluginOperationState::Unknown
                })
                .count();
            Ok((p, unknown_ops))
        })
        .await?;

        let active_workers = self
            .maybe_runtime()
            .map_or(0, |rt| rt.active_plugin_count(plugin_id));
        let in_flight = self.in_flight().active_count(plugin_id);
        let snap = self.in_flight().metrics_for(plugin_id);
        let avg_latency_ms = if snap.total_invocations > 0 {
            snap.total_latency_ms as f64 / snap.total_invocations as f64
        } else {
            0.0
        };
        Ok(PluginMetricsView {
            plugin_id: plugin_id.to_string(),
            window_secs: window_secs.unwrap_or(60),
            active_workers,
            in_flight_invocations: in_flight,
            draining: plugin.lifecycle == PluginLifecycle::Draining,
            unknown_operations: unknown_ops,
            total_invocations: snap.total_invocations,
            total_errors: snap.total_errors,
            avg_latency_ms,
            max_latency_ms: snap.max_latency_ms,
        })
    }
}

enum ResumePhase1 {
    Replayed(Box<OperationAdminView>),
    NonStaged {
        view: OperationAdminView,
        updated: store::plugins::PluginOperation,
    },
    Staged {
        kind: String,
        claim: store::plugins::PluginCommandClaim,
        intent: store::plugins::StagedPluginIntent,
        now: i64,
    },
}

fn begin_resume_in_app(
    app: &store::App<'_>,
    op_id: Uuid,
    actor: domain::UserId,
    rev: i64,
    idem: &str,
    req: &store::plugins::CanonicalPluginRequest,
) -> Result<ResumePhase1, PluginServiceError> {
    let subject = PluginSubject::User(actor);
    if let Some(val) =
        app.get_plugin_idempotency_envelope(&subject, "operation-resume", idem, req)?
    {
        let replayed = serde_json::from_value::<OperationAdminView>(val)
            .map_err(|e| PluginServiceError::Io(e.to_string()))?;
        return Ok(ResumePhase1::Replayed(Box::new(replayed)));
    }
    let resume_ctx = ResumeIdempotencyCtx {
        actor,
        idempotency_key: idem.to_string(),
        request: req.clone(),
    };
    let matched_claim =
        super::idempotency::check_resume_claim_conflict_in_app(app, op_id, &resume_ctx)?;
    let admin = app.get_plugin_operation_admin(op_id)?.ok_or_else(|| {
        PluginServiceError::not_found("plugin.operation_missing", "operation not found")
    })?;
    let same_active_claim = super::idempotency::matches_active_resume_claim(
        &admin.operation,
        matched_claim.as_ref(),
        rev,
    );
    if admin.operation.revision != rev && !same_active_claim {
        return Err(PluginServiceError::conflict(
            "plugin.revision_conflict",
            "operation revision mismatch",
        ));
    }
    let Some(intent) = admin.intent else {
        let updated = app.resume_plugin_command(&subject, op_id, rev)?;
        let view = load_app_operation_admin_view(app, op_id)?;
        let json =
            serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        app.put_plugin_idempotency_envelope(&subject, "operation-resume", idem, req, &json)?;
        return Ok(ResumePhase1::NonStaged { view, updated });
    };

    let op = &admin.operation;
    let resumable = op.phase != "cancel-requested"
        && (matches!(
            op.state,
            PluginOperationState::Queued
                | PluginOperationState::Deferred
                | PluginOperationState::Running
                | PluginOperationState::Unknown
        ) || (op.state == PluginOperationState::Blocked && op.phase == "drain-pending"));
    if !resumable {
        return Err(PluginServiceError::conflict(
            "plugin.conflict",
            "staged operation is not resumable",
        ));
    }
    let now = now_unix();
    let claim = match op.state {
        PluginOperationState::Queued | PluginOperationState::Deferred => {
            app.claim_staged_plugin_operation(op.operation_id, op.revision, admin.attempt, now)?
        }
        _ => app.recover_staged_plugin_operation(
            op.operation_id,
            op.revision,
            admin.attempt,
            "resuming",
            now,
        )?,
    };
    save_resume_claim_meta_in_app(app, op.operation_id, &resume_ctx, rev, claim.revision())?;
    Ok(ResumePhase1::Staged {
        kind: admin.operation.kind,
        claim,
        intent,
        now,
    })
}

async fn finish_staged_resume(
    state: &ApiState,
    op_id: Uuid,
    kind: String,
    mut claim: store::plugins::PluginCommandClaim,
    intent: store::plugins::StagedPluginIntent,
    now: i64,
    input: ResumeOperationInput,
) -> Result<OperationAdminView, PluginServiceError> {
    let resume_ctx = ResumeIdempotencyCtx {
        actor: input.actor_user_id,
        idempotency_key: input.idempotency_key,
        request: input.request,
    };
    let s = state.clone();
    run_blocking(move || {
        recover_claimed_staged_operation(
            &s,
            &kind,
            &mut claim,
            &intent,
            Some(resume_ctx.actor),
            now,
            true,
            Some(&resume_ctx),
        )?;
        if let Some(saved) = s.store.get_plugin_idempotency_envelope(
            &PluginSubject::User(resume_ctx.actor),
            "operation-resume",
            &resume_ctx.idempotency_key,
            &resume_ctx.request,
        )? {
            return serde_json::from_value(saved)
                .map_err(|e| PluginServiceError::Io(e.to_string()));
        }
        load_operation_admin_view(&s.store, op_id)
    })
    .await
}

async fn project_resumed_non_staged_job(
    state: &ApiState,
    updated: store::plugins::PluginOperation,
) -> Result<(), PluginServiceError> {
    let s = state.clone();
    run_blocking(move || {
        if matches!(
            updated.state,
            PluginOperationState::Queued | PluginOperationState::Deferred
        ) && let Err(error) = project_command_job(&s, &updated)
        {
            if matches!(error, PluginServiceError::Store(store::StoreError::WriteBusy)) {
                tracing::warn!(
                    operation_id = %updated.operation_id,
                    "Jobs writer busy during foreground plugin command resume projection; deferred to reconciliation"
                );
            } else {
                return Err(error);
            }
        }
        Ok(())
    })
    .await
}

fn is_replayable_resume_conflict(err: &PluginServiceError) -> bool {
    match err {
        PluginServiceError::Conflict { code, .. } => *code != "instance.read_only",
        PluginServiceError::Store(store::StoreError::Plugin(
            PluginError::StaleAuthority | PluginError::Conflict,
        )) => true,
        _ => false,
    }
}

fn is_replayable_admin_cmd_conflict(err: &PluginServiceError) -> bool {
    match err {
        PluginServiceError::Conflict { code, .. } => *code != "instance.read_only",
        PluginServiceError::Forbidden {
            code: "plugin.cleanup_token_invalid",
            ..
        } => true,
        PluginServiceError::Store(store::StoreError::Plugin(
            PluginError::StaleAuthority | PluginError::Conflict,
        )) => true,
        _ => false,
    }
}

fn accept_admin_command_in_store(
    state: &ApiState,
    store: &store::Store,
    pid: &str,
    mut input: SubmitAdminCommandInput,
) -> Result<AcceptedPluginOperation, PluginServiceError> {
    let p = store
        .get_plugin(pid)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin not found"))?;
    if p.lifecycle != PluginLifecycle::Enabled || !p.desired_enabled {
        return Err(PluginServiceError::conflict(
            "plugin.not_enabled",
            "plugin must be enabled",
        ));
    }
    resolve_ui_command_handles(
        state,
        &p,
        input.actor_user_id,
        &input.kind,
        &mut input.payload,
    )?;
    let kind = normalize_admin_command_kind(store, pid, &input.kind);
    let targets = if input.targets.is_empty() {
        infer_admin_command_targets(store, pid, &kind, &input.payload)
    } else {
        input.targets
    };
    let auth = issue_admin_command_authority(store, &p, &targets)?;
    // Enforce capability grant BEFORE supports_kind (M3.1 Caveat #3)
    let command_auth = store.authorize_plugin_command(&auth, &kind, &targets)?;
    if !state.plugins.effects().supports_kind(&kind) {
        return Err(PluginServiceError::UnsupportedCommand(kind));
    }
    let payload_bytes = serde_json::to_vec(&input.payload)
        .map_err(|e| PluginServiceError::invalid("plugin.invalid_input", e.to_string()))?;
    super::command_targets::validate_admin_command_for_targets(
        state,
        input.actor_user_id,
        &p,
        &kind,
        &targets,
        &input.payload,
    )?;
    let accept_input = AcceptPluginOperation {
        plugin_id: Some(auth.plugin_id().to_string()),
        subject: PluginSubject::User(input.actor_user_id),
        idempotency_key: input.idempotency_key,
        request: input.request,
        kind,
        expected_revision: Some(input.expected_revision),
        expected_generation: Some(auth.generation()),
        cause: PluginCommandCause::root(
            input
                .evidence_ref
                .unwrap_or_else(|| format!("admin-cmd:{pid}")),
        ),
    };
    Ok(store.accept_plugin_command(&command_auth, &accept_input, &payload_bytes)?)
}

fn project_and_load_admin_command(
    state: &ApiState,
    accepted: AcceptedPluginOperation,
) -> Result<OperationAdminView, PluginServiceError> {
    if matches!(
        accepted.operation.state,
        PluginOperationState::Queued | PluginOperationState::Deferred
    ) && let Err(error) = project_command_job(state, &accepted.operation)
    {
        if matches!(
            error,
            PluginServiceError::Store(store::StoreError::WriteBusy)
        ) {
            tracing::warn!(
                operation_id = %accepted.operation.operation_id,
                "Jobs writer busy during foreground plugin command projection; deferred to reconciliation"
            );
        } else {
            return Err(error);
        }
    }
    load_operation_admin_view(&state.store, accepted.operation.operation_id)
}

fn issue_admin_command_authority(
    store: &store::Store,
    plugin: &PluginRecord,
    targets: &[store::plugins::PluginCommandTarget],
) -> Result<store::plugins::PluginStateAuthority, PluginServiceError> {
    let ver = store
        .get_plugin_version(&plugin.plugin_id, &plugin.package_hash)?
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "plugin version missing"))?;
    let site_obs = if ver.credential_bound {
        let [store::plugins::PluginCommandTarget::Site(sid)] = targets else {
            return Err(PluginServiceError::forbidden(
                "plugin.forbidden",
                "credential-bound plugin command requires a single site target",
            ));
        };
        Some(store.observe_site(*sid)?.ok_or_else(|| {
            PluginServiceError::not_found("site.missing", "target site not found")
        })?)
    } else {
        None
    };
    Ok(store.issue_plugin_state_authority(plugin, site_obs.as_ref())?)
}

pub(crate) fn effect_label(effect: PluginEffectObservation) -> String {
    match effect {
        PluginEffectObservation::NoEffect => "none".into(),
        PluginEffectObservation::Confirmed => "confirmed".into(),
        PluginEffectObservation::Unknown => "unknown".into(),
    }
}
