pub mod fetcher;
mod helpers;
mod outcome;
pub(crate) mod plugin_command;
pub(crate) mod plugin_command_cleanup;
pub(crate) mod plugin_command_ops;
pub(crate) mod plugin_command_validate;
pub(crate) mod reconcile;
mod source;

use std::str::FromStr;

use async_trait::async_trait;
use domain::DownloaderId;
use downloader::{
    DownloadOpContext, DownloadOutcome, DownloadSubmitError, DownloadSubmitter, FrozenDownloadPlan,
};
use helpers::{map_store_error, purpose_label};
use hooks::Gate;
use outcome::{build_submit_options, record_gate_defer, record_gate_reject, settle_submit_result};
use plugin_api::DownloadPlanView;
use source::{
    PreparedSubmissionPayload, SubmissionFence, check_submission_fence,
    fetch_and_prepare_payload_at, snapshot_inputs,
};
use store::{
    AcceptDownloadOperationInput, CommitBoostFinalInput, DownloadOperationRecord,
    DownloadOperationState, DownloadOwnershipState, ReserveBoostProvisionalInput,
    TransitionDownloadOperationInput,
};

use crate::delivery;
use crate::management::ApiState;
use crate::plugins::hooks_runtime;

pub(crate) use helpers::{compute_request_digest, replay_outcome};
pub(crate) use plugin_command::{execute_plugin_command_step, reconcile_plugin_command_step};
pub(crate) use reconcile::reconcile_pending_operations;
pub(crate) use source::is_operation_rolled_back;

#[derive(Clone)]
pub struct ApiDownloadService {
    state: ApiState,
    expected_generation: Option<u64>,
    fence: Option<SubmissionFence>,
}

impl ApiDownloadService {
    pub fn new(state: ApiState) -> Self {
        Self {
            state,
            expected_generation: None,
            fence: None,
        }
    }

    pub fn with_expected_generation(mut self, expected_generation: Option<u64>) -> Self {
        self.expected_generation = expected_generation;
        self
    }

    pub(crate) fn with_fence(mut self, fence: SubmissionFence) -> Self {
        self.fence = Some(fence);
        self
    }

    pub fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        self.submit_sync_with_budget_cap(plan, op, None)
    }

    pub(crate) fn submit_sync_with_budget_cap(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
        budget_cap: Option<i64>,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        helpers::validate_initial_plan_inputs(&plan)?;
        check_submission_fence(&self.state, &plan, self.fence.as_ref())?;
        let now = crate::job_loop::unix_now();
        let (snap, subscribe_binding) = snapshot_inputs(&self.state, &plan)?;
        let initial_dl_id = snap.resolved_downloader_id;
        let request_digest = compute_request_digest(&plan, initial_dl_id);
        let candidate = hooks_runtime::torrent_to_candidate(
            &plan.torrent,
            op.operation_id.to_string(),
            snap.site_provider.as_deref().unwrap_or("indexer"),
        );
        let (final_view, gate, db_traces) = hooks_runtime::run_download_before(
            &self.state,
            plan.subscribe_id,
            candidate,
            purpose_label(plan.purpose),
            initial_plan_view(&plan, initial_dl_id),
            &snap.allowed_downloaders,
        );
        let frozen_dl_id =
            DownloaderId::from_str(&final_view.downloader_id).unwrap_or(initial_dl_id);
        let endpoint_fingerprint = snap
            .endpoint_by_id
            .get(&frozen_dl_id)
            .cloned()
            .or_else(|| plan.frozen_endpoint.clone());
        if let Gate::Reject(reason) = gate {
            return record_gate_reject(
                &self.state,
                &plan,
                &op,
                &db_traces,
                frozen_dl_id,
                &endpoint_fingerprint,
                request_digest,
                reason,
                now,
            );
        }
        if let Gate::Defer { until_ms, reason } = gate {
            check_submission_fence(&self.state, &plan, self.fence.as_ref())?;
            return record_gate_defer(
                &self.state,
                &plan,
                &op,
                &db_traces,
                subscribe_binding.as_ref(),
                frozen_dl_id,
                &endpoint_fingerprint,
                request_digest,
                until_ms,
                reason,
                now,
            );
        }
        let ownership_tag = format!("cm-op:{}", op.operation_id);
        let accepted = accept_intent_op(
            &self.state,
            &plan,
            &op,
            None,
            frozen_dl_id,
            &endpoint_fingerprint,
            &ownership_tag,
            request_digest,
            now,
        )?;
        if accepted.replayed
            && let Some(outcome) = replay_outcome(&accepted.operation)
        {
            return Ok(outcome);
        }
        let current_op = accepted.operation;
        let _in_flight = self
            .state
            .enter_download_op_in_flight(current_op.operation_id.clone());
        let prepared = fetch_and_prepare_payload_at(
            &self.state,
            &plan,
            snap.site.as_ref(),
            &current_op,
            frozen_dl_id,
            endpoint_fingerprint.clone(),
            self.expected_generation,
            now,
        )?;
        if let Err(err) = check_submission_fence(&self.state, &plan, self.fence.as_ref()) {
            source::fail_intent_operation(
                &self.state,
                &current_op,
                frozen_dl_id,
                endpoint_fingerprint,
                "cancelled",
                err.to_string(),
                now,
            );
            return Err(err);
        }
        submit_prepared_to_client(
            &self.state,
            &plan,
            &op,
            &final_view,
            current_op,
            prepared,
            frozen_dl_id,
            endpoint_fingerprint,
            ownership_tag,
            budget_cap,
            now,
        )
    }
}

fn reject_boost_reservation_op(
    state: &ApiState,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: &PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    outcome_code: &str,
    reason: &str,
    now: i64,
) -> DownloadOutcome {
    if let Err(err) = state
        .store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: current_op.operation_id.clone(),
            expected_revision: current_op.revision,
            next_state: DownloadOperationState::Rejected,
            downloader_id: Some(frozen_dl_id),
            endpoint_fingerprint: current_op.endpoint_fingerprint.clone(),
            info_hash: Some(prepared.info_hash.clone()),
            bytes_digest: prepared.bytes_digest.clone(),
            verified_bytes: prepared.verified_bytes,
            outcome_code: Some(outcome_code.to_string()),
            error_message: Some(reason.to_string()),
            submitted_at: None,
            now_utc: now,
        })
    {
        tracing::warn!(
            operation_id = %current_op.operation_id,
            error = %err,
            "failed to transition rejected boost download operation"
        );
    }
    DownloadOutcome::Rejected {
        operation_id: op.operation_id,
        reason: reason.to_string(),
        broken_task_hash: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn reserve_provisional_or_reject(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: &PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    quota_key: String,
    plugin_id: String,
    verified_bytes: i64,
    now: i64,
) -> Result<Result<(store::BoostReservationRecord, bool), DownloadOutcome>, DownloadSubmitError> {
    if let Some(existing) = state
        .store
        .get_boost_reservation_by_operation(&current_op.operation_id)
        .map_err(map_store_error)?
    {
        return Ok(Ok((existing, false)));
    }
    match state
        .store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: format!("res-{}", current_op.operation_id),
            operation_id: current_op.operation_id.clone(),
            quota_key,
            plugin_id,
            site_id: (!plan.torrent.site_id.as_uuid().is_nil()).then_some(plan.torrent.site_id),
            downloader_id: frozen_dl_id,
            info_hash: Some(prepared.info_hash.clone()),
            estimated_bytes: verified_bytes,
            expires_at: Some(now + 3600),
            now_utc: now,
        }) {
        Ok(r) => Ok(Ok((r, true))),
        Err(store::StoreError::Plugin(store::plugins::PluginError::Conflict)) => {
            Ok(Err(reject_boost_reservation_op(
                state,
                op,
                current_op,
                prepared,
                frozen_dl_id,
                "duplicate_active_reservation",
                "active boost reservation already exists for info_hash",
                now,
            )))
        }
        Err(err) => Err(map_store_error(err)),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_boost_payload_guards(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: &PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    verified_bytes: i64,
    now: i64,
) -> Option<DownloadOutcome> {
    if let Some(advertised) = plan.torrent.size_bytes
        && advertised > 0
        && u64::try_from(verified_bytes.max(0)).unwrap_or(u64::MAX)
            > advertised
                .saturating_mul(16)
                .max(advertised.saturating_add(10 * 1024 * 1024))
    {
        return Some(reject_boost_reservation_op(
            state,
            op,
            current_op,
            prepared,
            frozen_dl_id,
            "advertised_size_mismatch",
            "verified torrent payload size disagrees with advertised size",
            now,
        ));
    }
    if verified_bytes > 0
        && let Ok(client) = delivery::frozen_client_for_id(state, Some(frozen_dl_id))
        && let Ok(downloader::DiskSpaceInfo {
            status: downloader::DiskSpaceStatus::Available,
            available_bytes: Some(free_bytes),
            ..
        }) = client.disk_free_space()
        && u64::try_from(verified_bytes).unwrap_or(u64::MAX) > free_bytes
    {
        return Some(reject_boost_reservation_op(
            state,
            op,
            current_op,
            prepared,
            frozen_dl_id,
            "insufficient_disk_space",
            "insufficient downloader free disk space for boost torrent",
            now,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn prepare_boost_reservation(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: &PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    budget_cap: Option<i64>,
    now: i64,
) -> Result<Result<DownloadOperationRecord, DownloadOutcome>, DownloadSubmitError> {
    let plugin_id = op
        .plugin_id
        .clone()
        .unwrap_or_else(|| crate::plugins::boost::BUILTIN_BOOST_PLUGIN_ID.to_string());
    let quota_key = plan
        .budget_key
        .clone()
        .unwrap_or_else(|| format!("{plugin_id}:{frozen_dl_id}"));
    let cfg_val = state
        .store
        .get_plugin_config(&plugin_id)
        .ok()
        .flatten()
        .map(|c| c.values)
        .unwrap_or_else(|| serde_json::json!({}));
    let cfg = crate::plugins::boost::config::BuiltinBoostConfig::from_layers(&[&cfg_val])
        .unwrap_or_default();
    let budget_bytes = match (cfg.budget_bytes, budget_cap) {
        (Some(cfg_b), Some(cap_b)) => cfg_b.min(cap_b.max(0)),
        (Some(cfg_b), None) => cfg_b,
        (None, Some(cap_b)) => cap_b.max(0),
        (None, None) => i64::MAX / 2,
    };
    let max_add_per_hour = Some(cfg.max_adds_per_hour);
    let verified_bytes = prepared.verified_bytes.unwrap_or(0);
    if let Some(rejected) = validate_boost_payload_guards(
        state,
        plan,
        op,
        current_op,
        prepared,
        frozen_dl_id,
        verified_bytes,
        now,
    ) {
        return Ok(Err(rejected));
    }
    if budget_bytes <= 0 {
        return Ok(Err(reject_boost_reservation_op(
            state,
            op,
            current_op,
            prepared,
            frozen_dl_id,
            "quota_exceeded",
            "boost quota exceeded",
            now,
        )));
    }
    let bytes_digest = prepared
        .bytes_digest
        .clone()
        .unwrap_or_else(|| helpers::sha256_hex(prepared.info_hash.as_bytes()));

    let (res, newly_reserved) = match reserve_provisional_or_reject(
        state,
        plan,
        op,
        current_op,
        prepared,
        frozen_dl_id,
        quota_key,
        plugin_id,
        verified_bytes,
        now,
    )? {
        Ok(pair) => pair,
        Err(rejected) => return Ok(Err(rejected)),
    };

    match state
        .store
        .commit_boost_final_reservation(&CommitBoostFinalInput {
            operation_id: current_op.operation_id.clone(),
            expected_reservation_revision: res.revision,
            info_hash: prepared.info_hash.clone(),
            bytes_digest,
            verified_bytes,
            budget_bytes,
            max_add_per_hour,
            mark_operation_submitting: true,
            now_utc: now,
        }) {
        Ok(_) => {
            let refreshed = state
                .store
                .get_download_operation(&current_op.operation_id)
                .map_err(map_store_error)?
                .ok_or_else(|| {
                    DownloadSubmitError::Storage("download operation missing after commit".into())
                })?;
            Ok(Ok(refreshed))
        }
        Err(err) => handle_commit_boost_error(
            state,
            op,
            current_op,
            prepared,
            frozen_dl_id,
            res.revision,
            newly_reserved,
            now,
            err,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_commit_boost_error(
    state: &ApiState,
    op: &DownloadOpContext,
    current_op: &DownloadOperationRecord,
    prepared: &PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    res_revision: i64,
    newly_reserved: bool,
    now: i64,
    err: store::StoreError,
) -> Result<Result<DownloadOperationRecord, DownloadOutcome>, DownloadSubmitError> {
    let release = || {
        if let Err(err) =
            state
                .store
                .release_boost_reservation(&current_op.operation_id, res_revision, now)
        {
            tracing::warn!(
                operation_id = %current_op.operation_id,
                error = %err,
                "failed to release provisional boost reservation"
            );
        }
    };
    match err {
        store::StoreError::Plugin(store::plugins::PluginError::QuotaExceeded) => {
            release();
            Ok(Err(reject_boost_reservation_op(
                state,
                op,
                current_op,
                prepared,
                frozen_dl_id,
                "quota_exceeded",
                "boost quota exceeded",
                now,
            )))
        }
        store::StoreError::Plugin(store::plugins::PluginError::Conflict)
            if newly_reserved && current_op.downloader_id.is_none_or(|d| d == frozen_dl_id) =>
        {
            release();
            Ok(Err(reject_boost_reservation_op(
                state,
                op,
                current_op,
                prepared,
                frozen_dl_id,
                "duplicate_active_reservation",
                "active boost reservation already exists for info_hash",
                now,
            )))
        }
        other => {
            if newly_reserved {
                release();
            }
            Err(map_store_error(other))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_prepared_to_client(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    final_view: &DownloadPlanView,
    current_op: DownloadOperationRecord,
    prepared: PreparedSubmissionPayload,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    ownership_tag: String,
    budget_cap: Option<i64>,
    now: i64,
) -> Result<DownloadOutcome, DownloadSubmitError> {
    let client = source::validate_and_connect_pre_send_client(
        state,
        plan,
        &current_op,
        frozen_dl_id,
        &endpoint_fingerprint,
        now,
    )?;
    let is_boost =
        plan.purpose == downloader::SubmissionPurpose::Boost || plan.budget_key.is_some();
    let current_op = if is_boost {
        match prepare_boost_reservation(
            state,
            plan,
            op,
            &current_op,
            &prepared,
            frozen_dl_id,
            budget_cap,
            now,
        )? {
            Ok(updated) => updated,
            Err(rejected) => return Ok(rejected),
        }
    } else {
        state
            .store
            .transition_download_operation(&TransitionDownloadOperationInput {
                operation_id: current_op.operation_id.clone(),
                expected_revision: current_op.revision,
                next_state: DownloadOperationState::Submitting,
                downloader_id: Some(frozen_dl_id),
                endpoint_fingerprint: endpoint_fingerprint.clone(),
                info_hash: Some(prepared.info_hash.clone()),
                bytes_digest: prepared.bytes_digest.clone(),
                verified_bytes: prepared.verified_bytes,
                outcome_code: None,
                error_message: None,
                submitted_at: None,
                now_utc: now,
            })
            .map_err(map_store_error)?
    };
    let existing_owned_tag = state
        .store
        .find_download_ownership_by_hash(frozen_dl_id, &prepared.info_hash)
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|r| r.ownership_state == DownloadOwnershipState::Owned)
                .map(|r| r.ownership_tag)
        });
    let effective_tag = existing_owned_tag
        .clone()
        .unwrap_or_else(|| ownership_tag.clone());
    let submit_opts = build_submit_options(plan, final_view, effective_tag);
    let submit_res = client.submit_verified(&prepared.submit_payload, &submit_opts);
    let done_now = crate::job_loop::unix_now();

    settle_submit_result(
        state,
        client.as_ref(),
        plan,
        op,
        &current_op,
        prepared,
        frozen_dl_id,
        endpoint_fingerprint,
        ownership_tag,
        existing_owned_tag,
        final_view.paused,
        submit_res,
        done_now,
    )
}

fn initial_plan_view(plan: &FrozenDownloadPlan, initial_dl_id: DownloaderId) -> DownloadPlanView {
    DownloadPlanView {
        downloader_id: initial_dl_id.to_string(),
        relative_directory: plan.save_path.clone(),
        category: plan.category.clone(),
        tags: plan.tags.clone(),
        paused: plan.paused,
        upload_limit: plan.upload_limit,
        download_limit: plan.download_limit,
        sequential: plan.sequential,
        first_last_piece: plan.first_last_piece,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn accept_intent_op(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    op: &DownloadOpContext,
    plugin_id_override: Option<String>,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: &Option<String>,
    ownership_tag: &str,
    request_digest: String,
    now: i64,
) -> Result<store::AcceptedDownloadOperation, DownloadSubmitError> {
    let resolved_plugin_id = plugin_id_override.or_else(|| op.plugin_id.clone());
    for attempt in 0usize.. {
        let idem_key = if attempt == 0 {
            op.idempotency_key.clone()
        } else {
            format!("{}#rb:{attempt}", op.idempotency_key)
        };
        let accepted = state
            .store
            .accept_or_retry_download_operation(&AcceptDownloadOperationInput {
                operation_id: op.operation_id.to_string(),
                idempotency_key: idem_key,
                source_kind: purpose_label(plan.purpose).to_string(),
                subscribe_id: plan.subscribe_id,
                plugin_id: resolved_plugin_id.clone(),
                subject: op.subject.clone(),
                cause_id: op.cause_id.clone(),
                site_id: Some(plan.torrent.site_id),
                downloader_id: Some(frozen_dl_id),
                endpoint_fingerprint: endpoint_fingerprint.clone(),
                enclosure: plan.torrent.enclosure.clone(),
                ownership_tag: ownership_tag.to_string(),
                info_hash: None,
                bytes_digest: None,
                verified_bytes: None,
                request_digest: request_digest.clone(),
                now_utc: now,
            })
            .map_err(map_store_error)?;
        if accepted.replayed && is_operation_rolled_back(state, &accepted.operation) {
            continue;
        }
        return Ok(accepted);
    }
    unreachable!()
}

impl subscribe::SyncDownloadSubmitter for ApiDownloadService {
    fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        ApiDownloadService::submit_sync(self, plan, op)
    }
}

pub(crate) struct LibraryCancelSubmitter {
    inner: ApiDownloadService,
    library_id: String,
    subscribe_id: domain::SubscribeId,
    expected_subscribe: Option<domain::Subscribe>,
    control: Option<library::WorkControl>,
}

impl LibraryCancelSubmitter {
    pub(crate) fn new(
        state: ApiState,
        library_id: String,
        subscribe_id: domain::SubscribeId,
        control: Option<library::WorkControl>,
    ) -> Self {
        Self {
            inner: ApiDownloadService::new(state),
            library_id,
            subscribe_id,
            expected_subscribe: None,
            control,
        }
    }

    pub(crate) fn with_expected_subscribe(
        mut self,
        expected_subscribe: Option<domain::Subscribe>,
    ) -> Self {
        self.expected_subscribe = expected_subscribe;
        self
    }
}

impl subscribe::SyncDownloadSubmitter for LibraryCancelSubmitter {
    fn submit_sync(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        self.inner
            .clone()
            .with_fence(SubmissionFence {
                library_id: Some(self.library_id.clone()),
                subscribe_id: Some(self.subscribe_id),
                expected_subscribe: self.expected_subscribe.clone(),
                control: self.control.clone(),
            })
            .submit_sync(plan, op)
    }
}

#[async_trait]
impl DownloadSubmitter for ApiDownloadService {
    async fn submit(
        &self,
        plan: FrozenDownloadPlan,
        op: DownloadOpContext,
    ) -> Result<DownloadOutcome, DownloadSubmitError> {
        let svc = self.clone();
        tokio::task::spawn_blocking(move || svc.submit_sync(plan, op))
            .await
            .map_err(|e| DownloadSubmitError::Storage(e.to_string()))?
    }
}
