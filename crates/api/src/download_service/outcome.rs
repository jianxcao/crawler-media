use domain::DownloaderId;
use downloader::{
    ClientSubmitResult, DownloadOpContext, DownloadOutcome, DownloadSubmitError, Downloader,
    FrozenDownloadPlan, SubmitOptions,
};
use hooks::{HookStepStatus, HookTrace};
use plugin_api::DownloadPlanView;
use store::{
    BoostProtectionState, BoostReservationStage, BoostTaskState, DownloadOperationRecord,
    DownloadOperationState, DownloadOwnershipState, DownloadProvenanceKind,
    ReconcileDownloadOperationInput, RecordDownloadOwnershipInput, RecordSubscribeDeferInput,
    StoreError, SubscribeDeferState, TransitionDownloadOperationInput, UpsertBoostTaskFactInput,
    plugins::PluginError,
};
use uuid::Uuid;

use super::helpers::{dedup_tags, map_store_error, replay_outcome, resolve_active_subscribe_defer};
use super::source::PreparedSubmissionPayload;
use crate::management::ApiState;

fn trace_plugin_id(
    traces: &[HookTrace],
    statuses: &[HookStepStatus],
    fallback: Option<&str>,
) -> Option<String> {
    traces
        .iter()
        .rev()
        .flat_map(|t| t.steps.iter().rev())
        .find(|s| statuses.contains(&s.status))
        .map(|s| s.plugin_id.clone())
        .or_else(|| fallback.map(str::to_string))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_gate_reject(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    traces: &[HookTrace],
    dl_id: DownloaderId,
    endpoint_fingerprint: &Option<String>,
    request_digest: String,
    reason: String,
    now: i64,
) -> Result<DownloadOutcome, DownloadSubmitError> {
    let attr_plugin_id = trace_plugin_id(
        traces,
        &[HookStepStatus::Rejected, HookStepStatus::Error],
        op.plugin_id.as_deref(),
    );
    let ownership_tag = format!("cm-op:{}", op.operation_id);
    let accepted = super::accept_intent_op(
        state,
        plan,
        op,
        attr_plugin_id,
        dl_id,
        endpoint_fingerprint,
        &ownership_tag,
        request_digest,
        now,
    )?;
    if accepted.replayed
        && let Some(outcome) = replay_outcome(&accepted.operation)
    {
        return Ok(outcome);
    }
    if accepted.operation.state == DownloadOperationState::Intent {
        state
            .store
            .transition_download_operation(&TransitionDownloadOperationInput {
                operation_id: accepted.operation.operation_id,
                expected_revision: accepted.operation.revision,
                next_state: DownloadOperationState::Rejected,
                downloader_id: Some(dl_id),
                endpoint_fingerprint: endpoint_fingerprint.clone(),
                info_hash: None,
                bytes_digest: None,
                verified_bytes: None,
                outcome_code: Some("gate_rejected".to_string()),
                error_message: Some(reason.clone()),
                submitted_at: None,
                now_utc: now,
            })
            .map_err(map_store_error)?;
    }
    Ok(DownloadOutcome::Rejected {
        operation_id: op.operation_id,
        reason,
        broken_task_hash: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_gate_defer(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    traces: &[HookTrace],
    subscribe_binding: Option<&store::SubscribePolicyBinding>,
    dl_id: DownloaderId,
    endpoint_fingerprint: &Option<String>,
    request_digest: String,
    until_ms: u64,
    reason: String,
    now: i64,
) -> Result<DownloadOutcome, DownloadSubmitError> {
    let wait_secs = i64::try_from((until_ms / 1000).max(1)).unwrap_or(60);
    let until_unix = now.saturating_add(wait_secs);
    let clean_reason = hooks::sanitize_reason(&reason);
    let attr_plugin_id = trace_plugin_id(
        traces,
        &[HookStepStatus::Deferred, HookStepStatus::Error],
        op.plugin_id.as_deref(),
    );
    let ownership_tag = format!("cm-op:{}", op.operation_id);
    let accepted = super::accept_intent_op(
        state,
        plan,
        op,
        attr_plugin_id.clone(),
        dl_id,
        endpoint_fingerprint,
        &ownership_tag,
        request_digest,
        now,
    )?;
    if accepted.replayed
        && let Some(outcome) = replay_outcome(&accepted.operation)
    {
        return Ok(outcome);
    }

    let mut cap_rejected = false;
    if let Some(binding) = subscribe_binding {
        match state
            .store
            .record_subscribe_defer(&RecordSubscribeDeferInput {
                defer_id: Uuid::new_v4().to_string(),
                subscribe_id: binding.subscribe_id,
                subscribe_generation: binding.jobs_sync_generation.clone(),
                policy_revision: binding.policy_revision,
                enclosure: plan.torrent.enclosure.clone(),
                info_hash: None,
                plugin_id: attr_plugin_id,
                hook_id: "download.before".to_string(),
                reason: clean_reason.clone(),
                next_retry_at: until_unix,
                now_utc: now,
            }) {
            Ok(rec) if rec.state == SubscribeDeferState::Rejected => cap_rejected = true,
            Err(StoreError::Plugin(PluginError::Denied)) => cap_rejected = true,
            Ok(_) | Err(StoreError::Plugin(PluginError::StaleAuthority)) => {}
            Err(err) => {
                tracing::warn!(%err, subscribe_id = %binding.subscribe_id, "record_subscribe_defer failed");
            }
        }
    }

    if accepted.operation.state == DownloadOperationState::Intent {
        let (next_state, outcome_code) = if cap_rejected {
            (DownloadOperationState::Rejected, "gate_defer_rejected")
        } else {
            (DownloadOperationState::Deferred, "gate_deferred")
        };
        state
            .store
            .transition_download_operation(&TransitionDownloadOperationInput {
                operation_id: accepted.operation.operation_id,
                expected_revision: accepted.operation.revision,
                next_state,
                downloader_id: Some(dl_id),
                endpoint_fingerprint: endpoint_fingerprint.clone(),
                info_hash: None,
                bytes_digest: None,
                verified_bytes: None,
                outcome_code: Some(outcome_code.to_string()),
                error_message: Some(clean_reason.clone()),
                submitted_at: None,
                now_utc: now,
            })
            .map_err(map_store_error)?;
    }

    if cap_rejected {
        Ok(DownloadOutcome::Rejected {
            operation_id: op.operation_id,
            reason: clean_reason,
            broken_task_hash: None,
        })
    } else {
        Ok(DownloadOutcome::Deferred {
            operation_id: op.operation_id,
            until_unix,
            reason: clean_reason,
        })
    }
}

pub(super) fn build_submit_options(
    plan: &FrozenDownloadPlan,
    final_view: &DownloadPlanView,
    effective_tag: String,
) -> SubmitOptions {
    let mut extra_tags = dedup_tags(&final_view.tags);
    let enc_tag = downloader::ownership_tag(&plan.torrent.enclosure);
    if !extra_tags.contains(&enc_tag) {
        extra_tags.push(enc_tag);
    }
    if let Some(sub_id) = plan.subscribe_id {
        let sub_tag = format!("cm-sub:{sub_id}");
        if !extra_tags.contains(&sub_tag) {
            extra_tags.push(sub_tag);
        }
    }
    SubmitOptions {
        ownership_tag: effective_tag,
        save_path: final_view.relative_directory.clone(),
        category: final_view.category.clone(),
        extra_tags,
        paused: final_view.paused,
        upload_limit: final_view.upload_limit,
        download_limit: final_view.download_limit,
        sequential: final_view.sequential,
        first_last_piece: final_view.first_last_piece,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn settle_submit_result(
    state: &ApiState,
    client: &dyn Downloader,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    ownership_tag: String,
    existing_owned_tag: Option<String>,
    final_paused: bool,
    submit_res: Result<ClientSubmitResult, downloader::DownloaderError>,
    done_now: i64,
) -> Result<DownloadOutcome, DownloadSubmitError> {
    let PreparedSubmissionPayload {
        info_hash,
        bytes_digest,
        verified_bytes,
        ..
    } = prepared;
    let reused_existing_owned_tag = existing_owned_tag.is_some();
    let effective_ownership_tag = existing_owned_tag
        .as_deref()
        .unwrap_or(&current_op.ownership_tag)
        .to_string();
    let effective_res =
        apply_rpc_below16_owned_fallback(client, existing_owned_tag.as_deref(), submit_res);

    let (outcome, _outcome_tag) = match effective_res {
        Ok(ClientSubmitResult::Added {
            already_owned,
            info_hash: resolved_hash,
        }) => settle_added_outcome(
            state,
            client,
            plan,
            op,
            current_op,
            frozen_dl_id,
            bytes_digest,
            verified_bytes,
            already_owned,
            reused_existing_owned_tag,
            &effective_ownership_tag,
            final_paused,
            resolved_hash,
            done_now,
        )?,
        Ok(ClientSubmitResult::ForeignDuplicate {
            info_hash: resolved_hash,
        }) => settle_rejected_outcome(
            state,
            op,
            current_op,
            frozen_dl_id,
            bytes_digest,
            verified_bytes,
            resolved_hash,
            "existing_foreign",
            "foreign duplicate already exists on downloader",
            DownloadOwnershipState::Foreign,
            DownloadProvenanceKind::ExistingForeign,
            false,
            done_now,
        )?,
        Ok(ClientSubmitResult::ExistingBroken {
            info_hash: resolved_hash,
        }) => settle_rejected_outcome(
            state,
            op,
            current_op,
            frozen_dl_id,
            bytes_digest,
            verified_bytes,
            resolved_hash,
            "existing_broken",
            "existing owned task is broken on downloader",
            DownloadOwnershipState::Broken,
            DownloadProvenanceKind::BrokenExisting,
            true,
            done_now,
        )?,
        Ok(ClientSubmitResult::UnknownOutcome { reason }) => record_unknown_outcome(
            state,
            plan,
            op,
            current_op,
            frozen_dl_id,
            endpoint_fingerprint,
            ownership_tag,
            reused_existing_owned_tag,
            info_hash,
            bytes_digest,
            verified_bytes,
            reason,
            done_now,
        )?,
        Err(err) => {
            return Err(settle_downloader_error(
                state,
                current_op,
                frozen_dl_id,
                info_hash,
                bytes_digest,
                verified_bytes,
                err,
                done_now,
            ));
        }
    };

    Ok(outcome)
}

fn apply_rpc_below16_owned_fallback(
    client: &dyn Downloader,
    existing_owned_tag: Option<&str>,
    submit_res: Result<ClientSubmitResult, downloader::DownloaderError>,
) -> Result<ClientSubmitResult, downloader::DownloaderError> {
    match submit_res {
        Ok(ClientSubmitResult::ForeignDuplicate {
            info_hash: resolved_hash,
        }) if !client.supports_labels() && existing_owned_tag.is_some() => {
            Ok(ClientSubmitResult::Added {
                already_owned: true,
                info_hash: resolved_hash,
            })
        }
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
fn settle_downloader_error(
    state: &ApiState,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    info_hash: String,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    err: downloader::DownloaderError,
    done_now: i64,
) -> DownloadSubmitError {
    if let Err(reconcile_err) = reconcile_outcome(
        state,
        current_op,
        frozen_dl_id,
        DownloadOperationState::Failed,
        Some(info_hash),
        Some("downloader_error".to_string()),
        Some(err.to_string()),
        None,
        bytes_digest,
        verified_bytes,
        done_now,
    ) {
        tracing::warn!(
            operation_id = %current_op.operation_id,
            error = %reconcile_err,
            "failed to reconcile failed download operation"
        );
    }
    DownloadSubmitError::Downloader(err)
}

#[allow(clippy::too_many_arguments)]
fn build_added_boost_task_fact(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    effective_ownership_tag: &str,
    final_paused: bool,
    verified_bytes: Option<i64>,
    resolved_hash: &str,
    done_now: i64,
) -> Option<UpsertBoostTaskFactInput> {
    if plan.purpose != downloader::SubmissionPurpose::Boost && plan.budget_key.is_none() {
        return None;
    }
    let plugin_id = op.plugin_id.clone().or_else(|| {
        plan.budget_key
            .as_deref()
            .and_then(|k| k.rsplit_once(':').map(|(p, _)| p.to_string()))
            .filter(|p| !p.trim().is_empty())
    })?;
    let quota_key = plan
        .budget_key
        .clone()
        .unwrap_or_else(|| crate::plugins::boost::config::quota_key(&plugin_id, frozen_dl_id));
    let cfg_val = state
        .store
        .get_plugin_config(&plugin_id)
        .ok()
        .flatten()
        .map(|c| c.values)
        .unwrap_or_else(|| serde_json::json!({}));
    let cfg =
        crate::plugins::boost::config::BoostConfig::from_layers(&[&cfg_val]).unwrap_or_default();
    let site_id = (!plan.torrent.site_id.as_uuid().is_nil()).then_some(plan.torrent.site_id);
    let hold_days = cfg.effective_hold_days(site_id);
    let hold_until = (hold_days > 0).then(|| done_now + hold_days * 86_400);
    let hr_active = plan.torrent.hr;
    let (protection_state, protection_reason) = if hr_active == Some(true) {
        (BoostProtectionState::Protected, Some("hr-active".into()))
    } else if hold_until.is_some() {
        (BoostProtectionState::Protected, Some("hold-active".into()))
    } else if hr_active == Some(false) {
        (BoostProtectionState::Eligible, None)
    } else {
        (BoostProtectionState::Unknown, Some("hr-unknown".into()))
    };
    let state_enum = if final_paused {
        BoostTaskState::Paused
    } else {
        BoostTaskState::Downloading
    };
    Some(UpsertBoostTaskFactInput {
        task_fact_id: format!("tf-{}", current_op.operation_id),
        plugin_id,
        quota_key,
        downloader_id: frozen_dl_id,
        ownership_tag: effective_ownership_tag.to_string(),
        info_hash: resolved_hash.to_string(),
        site_id,
        state: state_enum,
        occupied_bytes: verified_bytes.unwrap_or(0),
        retained_bytes: 0,
        hr_active,
        hr_until: None,
        hold_until,
        free_until: plan.torrent.free_until,
        protection_state,
        protection_reason,
        observed_at: done_now,
        expected_revision: None,
        now_utc: done_now,
    })
}

#[allow(clippy::too_many_arguments)]
fn settle_added_outcome(
    state: &ApiState,
    client: &dyn Downloader,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    already_owned: bool,
    reused_existing_owned_tag: bool,
    effective_ownership_tag: &str,
    final_paused: bool,
    resolved_hash: String,
    done_now: i64,
) -> Result<(DownloadOutcome, &'static str), DownloadSubmitError> {
    if let Err(err) = client.remember_identity(&plan.torrent, &resolved_hash) {
        tracing::warn!(
            operation_id = %current_op.operation_id,
            info_hash = %resolved_hash,
            error = %err,
            "failed to remember downloader task identity"
        );
    }
    let provenance = if already_owned {
        DownloadProvenanceKind::ExistingOwned
    } else {
        DownloadProvenanceKind::CreatedFresh
    };
    let code = if already_owned {
        "existing_owned"
    } else {
        "submitted"
    };
    let ownership =
        (!reused_existing_owned_tag).then_some((DownloadOwnershipState::Owned, provenance));
    let boost_task_fact = build_added_boost_task_fact(
        state,
        plan,
        op,
        current_op,
        frozen_dl_id,
        effective_ownership_tag,
        final_paused,
        verified_bytes,
        &resolved_hash,
        done_now,
    );

    reconcile_outcome_with_boost(
        state,
        current_op,
        frozen_dl_id,
        DownloadOperationState::Submitted,
        Some(resolved_hash.clone()),
        Some(code.to_string()),
        None,
        ownership,
        boost_task_fact,
        bytes_digest.clone(),
        verified_bytes,
        done_now,
    )?;
    if let Some(sub_id) = plan.subscribe_id {
        resolve_active_subscribe_defer(&state.store, sub_id, &plan.torrent.enclosure, done_now);
    }
    Ok((
        DownloadOutcome::Submitted {
            operation_id: op.operation_id,
            downloader_id: Some(frozen_dl_id),
            info_hash: Some(resolved_hash),
            bytes_digest: current_op.bytes_digest.clone().or(bytes_digest),
            verified_total_bytes: verified_bytes,
            already_owned,
        },
        "submitted",
    ))
}

#[allow(clippy::too_many_arguments)]
fn settle_rejected_outcome(
    state: &ApiState,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    resolved_hash: String,
    code: &str,
    error_message: &str,
    ownership: DownloadOwnershipState,
    provenance: DownloadProvenanceKind,
    include_broken_hash: bool,
    done_now: i64,
) -> Result<(DownloadOutcome, &'static str), DownloadSubmitError> {
    reconcile_outcome(
        state,
        current_op,
        frozen_dl_id,
        DownloadOperationState::Rejected,
        Some(resolved_hash.clone()),
        Some(code.to_string()),
        Some(error_message.to_string()),
        Some((ownership, provenance)),
        bytes_digest,
        verified_bytes,
        done_now,
    )?;
    Ok((
        DownloadOutcome::Rejected {
            operation_id: op.operation_id,
            reason: code.to_string(),
            broken_task_hash: include_broken_hash.then_some(resolved_hash),
        },
        "rejected",
    ))
}

#[allow(clippy::too_many_arguments)]
fn record_unknown_outcome(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    ownership_tag: String,
    reused_existing_owned_tag: bool,
    info_hash: String,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    reason: String,
    done_now: i64,
) -> Result<(DownloadOutcome, &'static str), DownloadSubmitError> {
    let mut op_for_event = current_op.clone();
    op_for_event.downloader_id = Some(frozen_dl_id);
    let outbox = crate::plugins::events::build_download_after_event(
        &op_for_event,
        DownloadOperationState::Unknown,
        Some(&info_hash),
        verified_bytes,
        done_now.saturating_mul(1_000),
    );
    state
        .store
        .transition_download_operation_with_outbox(
            &TransitionDownloadOperationInput {
                operation_id: current_op.operation_id.clone(),
                expected_revision: current_op.revision,
                next_state: DownloadOperationState::Unknown,
                downloader_id: Some(frozen_dl_id),
                endpoint_fingerprint,
                info_hash: Some(info_hash.clone()),
                bytes_digest: bytes_digest.clone(),
                verified_bytes,
                outcome_code: Some("unknown".to_string()),
                error_message: Some(reason.clone()),
                submitted_at: None,
                now_utc: done_now,
            },
            outbox.as_ref(),
        )
        .map_err(map_store_error)?;
    if outbox.is_some() {
        crate::plugins::events::wake(state);
    }
    if !reused_existing_owned_tag
        && let Err(err) = state
            .store
            .record_download_ownership(&RecordDownloadOwnershipInput {
                downloader_id: frozen_dl_id,
                ownership_tag,
                info_hash: Some(info_hash),
                operation_id: current_op.operation_id.clone(),
                subscribe_id: plan.subscribe_id,
                plugin_id: op.plugin_id.clone(),
                subject: op.subject.clone(),
                site_id: Some(plan.torrent.site_id),
                enclosure: plan.torrent.enclosure.clone(),
                ownership_state: DownloadOwnershipState::Unproven,
                provenance_kind: DownloadProvenanceKind::Unproven,
                verified_bytes,
                bytes_digest,
                now_utc: done_now,
            })
    {
        tracing::warn!(operation_id = %current_op.operation_id, error = %err, "failed to record unproven download ownership");
    }
    if let Ok(Some(res)) = state
        .store
        .get_boost_reservation_by_operation(&current_op.operation_id)
        && res.stage == BoostReservationStage::Final
        && let Err(err) = state.store.mark_boost_reservation_unknown_held(
            &current_op.operation_id,
            res.revision,
            done_now,
        )
    {
        tracing::warn!(
            operation_id = %current_op.operation_id,
            error = %err,
            "failed to mark boost reservation unknown-held"
        );
    }
    Ok((
        DownloadOutcome::Unknown {
            operation_id: op.operation_id,
            downloader_id: Some(frozen_dl_id),
            reason,
        },
        "unknown",
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_outcome(
    state: &ApiState,
    op: &DownloadOperationRecord,
    dl_id: DownloaderId,
    resolved_state: DownloadOperationState,
    info_hash: Option<String>,
    outcome_code: Option<String>,
    error_message: Option<String>,
    ownership: Option<(DownloadOwnershipState, DownloadProvenanceKind)>,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    now_utc: i64,
) -> Result<DownloadOperationRecord, DownloadSubmitError> {
    reconcile_outcome_with_boost(
        state,
        op,
        dl_id,
        resolved_state,
        info_hash,
        outcome_code,
        error_message,
        ownership,
        None,
        bytes_digest,
        verified_bytes,
        now_utc,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_outcome_with_boost(
    state: &ApiState,
    op: &DownloadOperationRecord,
    dl_id: DownloaderId,
    resolved_state: DownloadOperationState,
    info_hash: Option<String>,
    outcome_code: Option<String>,
    error_message: Option<String>,
    ownership: Option<(DownloadOwnershipState, DownloadProvenanceKind)>,
    boost_task_fact: Option<UpsertBoostTaskFactInput>,
    bytes_digest: Option<String>,
    verified_bytes: Option<i64>,
    now_utc: i64,
) -> Result<DownloadOperationRecord, DownloadSubmitError> {
    let own_rec =
        ownership.map(
            |(ownership_state, provenance_kind)| RecordDownloadOwnershipInput {
                downloader_id: dl_id,
                ownership_tag: op.ownership_tag.clone(),
                info_hash: info_hash.clone(),
                operation_id: op.operation_id.clone(),
                subscribe_id: op.subscribe_id,
                plugin_id: op.plugin_id.clone(),
                subject: op.subject.clone(),
                site_id: op.site_id,
                enclosure: op.enclosure.clone(),
                ownership_state,
                provenance_kind,
                verified_bytes,
                bytes_digest,
                now_utc,
            },
        );
    let mut op_for_event = op.clone();
    op_for_event.downloader_id = Some(dl_id);
    let outbox = crate::plugins::events::build_download_after_event(
        &op_for_event,
        resolved_state,
        info_hash.as_deref(),
        verified_bytes,
        now_utc.saturating_mul(1_000),
    );
    let rec = state
        .store
        .reconcile_download_operation_with_outbox(
            &ReconcileDownloadOperationInput {
                operation_id: op.operation_id.clone(),
                expected_revision: op.revision,
                resolved_state,
                info_hash,
                outcome_code,
                error_message,
                ownership: own_rec,
                boost_task_fact,
                now_utc,
            },
            outbox.as_ref(),
        )
        .map_err(map_store_error)?;
    if outbox.is_some() {
        crate::plugins::events::wake(state);
    }
    Ok(rec)
}
