use downloader::{ClientTaskQuery, DownloaderError};
use store::{
    DownloadOperationRecord, DownloadOperationState, DownloadOwnershipRecord,
    DownloadOwnershipState, DownloadProvenanceKind, ReconcileDownloadOperationInput,
    RecordDownloadOwnershipInput,
};

use super::helpers::resolve_active_subscribe_defer;
use crate::delivery;
use crate::management::ApiState;

pub(crate) fn reconcile_pending_operations(state: &ApiState, now: i64) -> Result<(), String> {
    let mut after: Option<String> = None;
    loop {
        let page = state
            .store
            .list_unresolved_download_operations(after.as_deref(), Some(64))
            .map_err(|e| e.to_string())?;
        let next = page.next_cursor.clone();
        for op in page.items {
            if matches!(
                op.state,
                DownloadOperationState::Unknown | DownloadOperationState::Submitting
            ) && !state.is_download_op_in_flight(&op.operation_id)
                && reconcile_operation_record(state, &op, now).is_none()
            {
                tracing::warn!(
                    operation_id = %op.operation_id,
                    state = ?op.state,
                    "pending download operation remained unresolved during reconciliation"
                );
            }
        }
        if next.is_none() || next == after {
            break;
        }
        after = next;
    }
    Ok(())
}

pub(crate) fn reconcile_download_operation_by_id(
    state: &ApiState,
    operation_id: &str,
    now: i64,
) -> Option<DownloadOperationRecord> {
    let op = state
        .store
        .get_download_operation(operation_id)
        .ok()
        .flatten()?;
    if !matches!(
        op.state,
        DownloadOperationState::Unknown | DownloadOperationState::Submitting
    ) || state.is_download_op_in_flight(&op.operation_id)
    {
        return Some(op);
    }
    reconcile_operation_record(state, &op, now)
}

fn has_prior_foreign_or_broken(
    store: &store::Store,
    dl_id: domain::DownloaderId,
    info_hash: Option<&str>,
) -> bool {
    let Some(hash) = info_hash else {
        return true;
    };
    match store.find_download_ownership_by_hash(dl_id, hash) {
        Ok(rows) => rows.iter().any(|r| {
            matches!(
                r.ownership_state,
                DownloadOwnershipState::Foreign | DownloadOwnershipState::Broken
            ) || matches!(
                r.provenance_kind,
                DownloadProvenanceKind::ExistingForeign | DownloadProvenanceKind::BrokenExisting
            )
        }),
        Err(_) => true,
    }
}

fn ownership_input(
    op: &DownloadOperationRecord,
    dl_id: domain::DownloaderId,
    resolved_hash: Option<String>,
    ownership_state: DownloadOwnershipState,
    provenance_kind: DownloadProvenanceKind,
    now: i64,
) -> RecordDownloadOwnershipInput {
    RecordDownloadOwnershipInput {
        downloader_id: dl_id,
        ownership_tag: op.ownership_tag.clone(),
        info_hash: resolved_hash,
        operation_id: op.operation_id.clone(),
        subscribe_id: op.subscribe_id,
        plugin_id: op.plugin_id.clone(),
        subject: op.subject.clone(),
        site_id: op.site_id,
        enclosure: op.enclosure.clone(),
        ownership_state,
        provenance_kind,
        verified_bytes: op.verified_bytes,
        bytes_digest: op.bytes_digest.clone(),
        now_utc: now,
    }
}

fn existing_owned_broken_input(
    existing: &DownloadOwnershipRecord,
    op: &DownloadOperationRecord,
    dl_id: domain::DownloaderId,
    resolved_hash: Option<String>,
    now: i64,
) -> RecordDownloadOwnershipInput {
    RecordDownloadOwnershipInput {
        downloader_id: dl_id,
        ownership_tag: existing.ownership_tag.clone(),
        info_hash: resolved_hash.or_else(|| existing.info_hash.clone()),
        operation_id: existing.operation_id.clone(),
        subscribe_id: existing.subscribe_id,
        plugin_id: existing.plugin_id.clone(),
        subject: existing.subject.clone(),
        site_id: existing.site_id,
        enclosure: existing.enclosure.clone(),
        ownership_state: DownloadOwnershipState::Broken,
        provenance_kind: DownloadProvenanceKind::BrokenExisting,
        verified_bytes: op.verified_bytes.or(existing.verified_bytes),
        bytes_digest: op
            .bytes_digest
            .clone()
            .or_else(|| existing.bytes_digest.clone()),
        now_utc: now,
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_reconciled_outcome<'a>(
    store: &store::Store,
    op: &'a DownloadOperationRecord,
    dl_id: domain::DownloaderId,
    supports_labels: bool,
    query: &ClientTaskQuery,
    existing_owned: Option<&'a DownloadOwnershipRecord>,
    matched_existing_tag: bool,
    resolved_hash: Option<String>,
    now: i64,
) -> (
    DownloadOperationState,
    Option<String>,
    Option<String>,
    Option<RecordDownloadOwnershipInput>,
    &'a str,
) {
    if !query.exists {
        return (
            DownloadOperationState::Failed,
            Some("reconciled_not_found".to_string()),
            Some("task not found on downloader during reconciliation".to_string()),
            None,
            &op.ownership_tag,
        );
    }
    let (is_owned, reused_existing) = if supports_labels {
        if matched_existing_tag {
            (true, existing_owned)
        } else {
            (query.owned_by_tag, None)
        }
    } else {
        let can_own = matches!(
            op.state,
            DownloadOperationState::Submitting | DownloadOperationState::Unknown
        ) && !has_prior_foreign_or_broken(store, dl_id, resolved_hash.as_deref());
        (can_own, if can_own { existing_owned } else { None })
    };
    if !is_owned {
        return (
            DownloadOperationState::Rejected,
            Some("existing_foreign".to_string()),
            Some("foreign task with same info_hash exists on downloader".to_string()),
            Some(ownership_input(
                op,
                dl_id,
                resolved_hash,
                DownloadOwnershipState::Foreign,
                DownloadProvenanceKind::ExistingForeign,
                now,
            )),
            &op.ownership_tag,
        );
    }
    let effective_tag = reused_existing
        .map(|r| r.ownership_tag.as_str())
        .unwrap_or(&op.ownership_tag);
    if query.broken {
        let broken_own = match reused_existing {
            Some(existing) => existing_owned_broken_input(existing, op, dl_id, resolved_hash, now),
            None => ownership_input(
                op,
                dl_id,
                resolved_hash,
                DownloadOwnershipState::Broken,
                DownloadProvenanceKind::BrokenExisting,
                now,
            ),
        };
        return (
            DownloadOperationState::Rejected,
            Some("existing_broken".to_string()),
            Some("existing owned task is broken on downloader".to_string()),
            Some(broken_own),
            effective_tag,
        );
    }
    let submitted_own = reused_existing.is_none().then(|| {
        ownership_input(
            op,
            dl_id,
            resolved_hash,
            DownloadOwnershipState::Owned,
            DownloadProvenanceKind::ReconciledOperation,
            now,
        )
    });
    (
        DownloadOperationState::Submitted,
        Some("reconciled_submitted".to_string()),
        None,
        submitted_own,
        effective_tag,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_reconciled_boost_task_fact(
    state: &ApiState,
    op: &DownloadOperationRecord,
    res: &store::BoostReservationRecord,
    dl_id: domain::DownloaderId,
    effective_ownership_tag: &str,
    hash: &str,
    paused: bool,
    now: i64,
) -> store::UpsertBoostTaskFactInput {
    let cfg_val = state
        .store
        .get_plugin_config(&res.plugin_id)
        .ok()
        .flatten()
        .map(|c| c.values)
        .unwrap_or_else(|| serde_json::json!({}));
    let cfg =
        crate::plugins::boost::config::BoostConfig::from_layers(&[&cfg_val]).unwrap_or_default();
    let hold_until = (cfg.hold_days > 0).then(|| now + cfg.hold_days * 86_400);
    let (protection_state, protection_reason) = if hold_until.is_some() {
        (
            store::BoostProtectionState::Protected,
            Some("hold-active".into()),
        )
    } else {
        (
            store::BoostProtectionState::Unknown,
            Some("hr-unknown-reconciled".into()),
        )
    };
    store::UpsertBoostTaskFactInput {
        task_fact_id: format!("tf-{}", op.operation_id),
        plugin_id: res.plugin_id.clone(),
        quota_key: res.quota_key.clone(),
        downloader_id: dl_id,
        ownership_tag: effective_ownership_tag.to_string(),
        info_hash: hash.to_string(),
        site_id: res.site_id.or(op.site_id),
        state: if paused {
            store::BoostTaskState::Paused
        } else {
            store::BoostTaskState::Downloading
        },
        occupied_bytes: res.reserved_bytes.max(0),
        retained_bytes: 0,
        hr_active: None,
        hr_until: None,
        hold_until,
        free_until: None,
        protection_state,
        protection_reason,
        observed_at: now,
        expected_revision: None,
        now_utc: now,
    }
}

fn query_for_reconcile(
    client: &dyn downloader::Downloader,
    op_id: &str,
    info_hash: &str,
    tag: &str,
) -> Option<ClientTaskQuery> {
    match client.query_task_status(info_hash, tag) {
        Ok(q) => Some(q),
        Err(DownloaderError::Message(msg)) => {
            tracing::warn!(operation_id = %op_id, error = %msg, "downloader unreachable during Unknown reconciliation");
            None
        }
        Err(err) => {
            tracing::warn!(operation_id = %op_id, error = %err, "downloader error during Unknown reconciliation");
            None
        }
    }
}

fn reconcile_operation_record(
    state: &ApiState,
    op: &DownloadOperationRecord,
    now: i64,
) -> Option<DownloadOperationRecord> {
    let dl_id = op.downloader_id?;
    if !dl_id.as_uuid().is_nil() && matches!(state.store.get_downloader(dl_id), Ok(None)) {
        return state
            .store
            .reconcile_download_operation(&ReconcileDownloadOperationInput {
                operation_id: op.operation_id.clone(),
                expected_revision: op.revision,
                resolved_state: DownloadOperationState::Failed,
                info_hash: op.info_hash.clone(),
                outcome_code: Some("downloader_deleted".to_string()),
                error_message: Some("target downloader no longer exists".to_string()),
                ownership: None,
                boost_task_fact: None,
                now_utc: now,
            })
            .ok();
    }
    let info_hash = op.info_hash.as_deref()?;
    let client = match delivery::frozen_client_for_id(state, Some(dl_id)) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(operation_id = %op.operation_id, error = %err, "downloader client unavailable for Unknown download reconciliation");
            return None;
        }
    };
    let existing_owned = state
        .store
        .find_download_ownership_by_hash(dl_id, info_hash)
        .ok()
        .and_then(|rows| {
            rows.into_iter().find(|r| {
                r.ownership_state == DownloadOwnershipState::Owned
                    && r.ownership_tag != op.ownership_tag
            })
        });
    let mut query = query_for_reconcile(
        client.as_ref(),
        &op.operation_id,
        info_hash,
        &op.ownership_tag,
    )?;
    let mut matched_existing_tag = false;
    if query.exists
        && !query.owned_by_tag
        && client.supports_labels()
        && let Some(ref existing) = existing_owned
    {
        let re_query = query_for_reconcile(
            client.as_ref(),
            &op.operation_id,
            info_hash,
            &existing.ownership_tag,
        )?;
        if re_query.owned_by_tag {
            matched_existing_tag = true;
            query = re_query;
        }
    }
    let resolved_hash = query.info_hash.clone().or_else(|| op.info_hash.clone());
    let (resolved_state, outcome_code, error_message, ownership, effective_ownership_tag) =
        classify_reconciled_outcome(
            &state.store,
            op,
            dl_id,
            client.supports_labels(),
            &query,
            existing_owned.as_ref(),
            matched_existing_tag,
            resolved_hash.clone(),
            now,
        );
    let boost_task_fact = if resolved_state == DownloadOperationState::Submitted
        && let Ok(Some(res)) = state
            .store
            .get_boost_reservation_by_operation(&op.operation_id)
        && let Some(ref hash) = resolved_hash
    {
        Some(build_reconciled_boost_task_fact(
            state,
            op,
            &res,
            dl_id,
            effective_ownership_tag,
            hash,
            query.paused,
            now,
        ))
    } else {
        None
    };
    persist_reconciled_operation(
        state,
        op,
        dl_id,
        resolved_state,
        resolved_hash,
        outcome_code,
        error_message,
        ownership,
        boost_task_fact,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_reconciled_operation(
    state: &ApiState,
    op: &DownloadOperationRecord,
    dl_id: domain::DownloaderId,
    resolved_state: DownloadOperationState,
    resolved_hash: Option<String>,
    outcome_code: Option<String>,
    error_message: Option<String>,
    ownership: Option<RecordDownloadOwnershipInput>,
    boost_task_fact: Option<store::UpsertBoostTaskFactInput>,
    now: i64,
) -> Option<DownloadOperationRecord> {
    let mut op_for_event = op.clone();
    op_for_event.downloader_id = Some(dl_id);
    let outbox = crate::plugins::events::build_download_after_event(
        &op_for_event,
        resolved_state,
        resolved_hash.as_deref(),
        ownership
            .as_ref()
            .and_then(|o| o.verified_bytes)
            .or(op.verified_bytes),
        now.saturating_mul(1_000),
    );
    let updated = match state.store.reconcile_download_operation_with_outbox(
        &ReconcileDownloadOperationInput {
            operation_id: op.operation_id.clone(),
            expected_revision: op.revision,
            resolved_state,
            info_hash: resolved_hash,
            outcome_code,
            error_message,
            ownership,
            boost_task_fact,
            now_utc: now,
        },
        outbox.as_ref(),
    ) {
        Ok(u) => u,
        Err(err) => {
            tracing::warn!(
                operation_id = %op.operation_id,
                error = %err,
                "failed to persist reconciled download operation"
            );
            return None;
        }
    };
    if outbox.is_some() {
        crate::plugins::events::wake(state);
    }
    if updated.state == DownloadOperationState::Submitted
        && let Some(sub_id) = updated.subscribe_id
    {
        resolve_active_subscribe_defer(&state.store, sub_id, &updated.enclosure, now);
    }
    Some(updated)
}
