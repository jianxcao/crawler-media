use std::sync::{Arc, OnceLock};

use domain::PluginOperationId;
use parking_lot::Mutex;
use plugin_api::{EndpointRequest, EndpointResponse, Failure};
use plugin_host::{Limits, PluginHost, PluginRuntime};
use store::Store;
use store::plugins::{
    CanonicalPluginRequest, PluginCommandCause, PluginCommandCursor, PluginCommandTarget,
    PluginRecord, PluginStateAuthority, PluginSubject,
};

use super::effect::{
    PluginEffectAdapter, PluginInvocationRole, PluginTaskRunner, ProductionEffectAdapter,
};
use super::ports::StoreHostPorts;
use super::reauth::{CleanupTokenStore, ConsentStore, ReauthService};
pub(crate) use super::service_exec::{
    map_store_call_error, run_app_read, run_app_write, run_blocking,
};
pub(super) use super::service_exec::{
    require_foreground_write_admitted, run_foreground_app_tx, run_foreground_store_sync,
};
use super::staging::{ActiveStagingPins, build_package_policy_from_store, stage_upload_sync};
use super::types::{PluginServiceError, StageUploadInput, StagedUploadView};
use super::wasm_runner::{
    ComponentCache, InFlightRegistry, WasmTaskRunner, invoke_endpoint as run_endpoint,
};
use crate::JobsWriter;
use crate::management::ApiState;

#[derive(Clone, Debug)]
pub struct SubmitPluginCommand {
    pub role: PluginInvocationRole,
    pub authority: PluginStateAuthority,
    pub subject: PluginSubject,
    pub kind: String,
    pub targets: Vec<PluginCommandTarget>,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
    pub expected_revision: Option<i64>,
    pub cause: PluginCommandCause,
    pub payload: Vec<u8>,
}

struct RuntimeGroup {
    runtime: PluginRuntime,
    wasm_host: PluginHost,
    validation_host: PluginHost,
}

type ResumeInFlightEntry = (
    CanonicalPluginRequest,
    Arc<tokio::sync::Notify>,
    Arc<std::sync::atomic::AtomicBool>,
);
type ResumeInFlightMap = std::collections::HashMap<(domain::UserId, String), ResumeInFlightEntry>;

pub(super) struct ResumeInFlightGuard<'a> {
    service: &'a PluginService,
    key: (domain::UserId, String),
}

impl Drop for ResumeInFlightGuard<'_> {
    fn drop(&mut self) {
        let removed = self.service.resume_in_flight.lock().remove(&self.key);
        if let Some((_, notify, done)) = removed {
            done.store(true, std::sync::atomic::Ordering::Release);
            notify.notify_waiters();
        }
    }
}

pub(super) struct ActiveStagedOpGuard {
    set: Arc<Mutex<std::collections::HashSet<uuid::Uuid>>>,
    op_id: uuid::Uuid,
}

impl Drop for ActiveStagedOpGuard {
    fn drop(&mut self) {
        self.set.lock().remove(&self.op_id);
    }
}

pub struct PluginService {
    effects: Arc<dyn PluginEffectAdapter>,
    task_runner: Arc<dyn PluginTaskRunner>,
    pre_effect_hook: Option<Arc<dyn Fn(PluginOperationId) + Send + Sync>>,
    tasks_cursor: Mutex<Option<uuid::Uuid>>,
    commands_cursor: Mutex<Option<PluginCommandCursor>>,
    unresolved_cursor: Mutex<Option<uuid::Uuid>>,
    ports: Arc<StoreHostPorts>,
    runtime: OnceLock<Result<RuntimeGroup, Failure>>,
    cache: ComponentCache,
    in_flight: Arc<InFlightRegistry>,
    reauth: Arc<ReauthService>,
    consent: Arc<ConsentStore>,
    cleanup_tokens: Arc<CleanupTokenStore>,
    staging_pins: Arc<ActiveStagingPins>,
    ui_handles: super::ui_handles::UiHandleSigner,
    hook_traces: Mutex<super::hooks_runtime::HookTraceBuffers>,
    events_notify: Arc<tokio::sync::Notify>,
    events_last_pruned_ms: std::sync::atomic::AtomicI64,
    fanout_read_fault: Mutex<
        Option<
            Arc<
                dyn Fn(super::events::FanoutReadOp<'_>) -> Result<(), store::StoreError>
                    + Send
                    + Sync,
            >,
        >,
    >,
    resume_in_flight: Mutex<ResumeInFlightMap>,
    active_staged_ops: Arc<Mutex<std::collections::HashSet<uuid::Uuid>>>,
}

impl Default for PluginService {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginService {
    pub fn new() -> Self {
        Self {
            effects: Arc::new(ProductionEffectAdapter::default()),
            task_runner: Arc::new(WasmTaskRunner),
            pre_effect_hook: None,
            tasks_cursor: Mutex::new(None),
            commands_cursor: Mutex::new(None),
            unresolved_cursor: Mutex::new(None),
            ports: Arc::new(StoreHostPorts::default()),
            runtime: OnceLock::new(),
            cache: ComponentCache::default(),
            in_flight: Arc::new(InFlightRegistry::default()),
            reauth: Arc::new(ReauthService::default()),
            consent: Arc::new(ConsentStore::default()),
            cleanup_tokens: Arc::new(CleanupTokenStore::default()),
            staging_pins: Arc::new(ActiveStagingPins::default()),
            ui_handles: super::ui_handles::UiHandleSigner::default(),
            hook_traces: Mutex::new(super::hooks_runtime::HookTraceBuffers::default()),
            events_notify: Arc::new(tokio::sync::Notify::new()),
            events_last_pruned_ms: std::sync::atomic::AtomicI64::new(i64::MIN),
            fanout_read_fault: Mutex::new(None),
            resume_in_flight: Mutex::new(ResumeInFlightMap::new()),
            active_staged_ops: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub(super) fn try_acquire_staged_op(&self, op_id: uuid::Uuid) -> Option<ActiveStagedOpGuard> {
        self.active_staged_ops
            .lock()
            .insert(op_id)
            .then(|| ActiveStagedOpGuard {
                set: self.active_staged_ops.clone(),
                op_id,
            })
    }

    pub(super) fn is_op_active_in_process(&self, op_id: uuid::Uuid) -> bool {
        self.active_staged_ops.lock().contains(&op_id)
    }

    pub(super) async fn acquire_resume_in_flight<'a>(
        &'a self,
        state: &ApiState,
        actor: domain::UserId,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
    ) -> Result<super::idempotency::OpBegin<ResumeInFlightGuard<'a>>, PluginServiceError> {
        let key = (actor, idempotency_key.to_string());
        loop {
            let waiter = {
                let mut map = self.resume_in_flight.lock();
                if let Some((existing_req, notify, done)) = map.get(&key) {
                    if existing_req != request {
                        return Err(PluginServiceError::conflict(
                            "plugin.idempotency_conflict",
                            "idempotency key reused with different request",
                        ));
                    }
                    Some((notify.clone(), done.clone()))
                } else {
                    let notify = Arc::new(tokio::sync::Notify::new());
                    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    map.insert(key.clone(), (request.clone(), notify, done));
                    None
                }
            };
            let Some((notify, done)) = waiter else {
                return Ok(super::idempotency::OpBegin::Proceed(ResumeInFlightGuard {
                    service: self,
                    key,
                }));
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !done.load(std::sync::atomic::Ordering::Acquire) {
                notified.await;
            }
            if let Some(replayed) =
                super::idempotency::check_idempotent_replay_after_write::<
                    super::types::OperationAdminView,
                >(state, actor, "operation-resume", idempotency_key, request)
                .await?
            {
                return Ok(super::idempotency::OpBegin::Replayed(Box::new(replayed)));
            }
        }
    }

    pub(crate) fn try_claim_event_prune_slot(&self, now_ms: i64, min_interval_ms: i64) -> bool {
        use std::sync::atomic::Ordering;
        let last = self.events_last_pruned_ms.load(Ordering::Relaxed);
        if last != i64::MIN && now_ms.saturating_sub(last) < min_interval_ms {
            return false;
        }
        self.events_last_pruned_ms
            .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub(crate) fn wake_events(&self) {
        self.events_notify.notify_one();
    }

    #[doc(hidden)]
    pub fn set_fanout_read_fault_for_test(
        &self,
        fault: Option<
            Arc<
                dyn Fn(super::events::FanoutReadOp<'_>) -> Result<(), store::StoreError>
                    + Send
                    + Sync,
            >,
        >,
    ) {
        *self.fanout_read_fault.lock() = fault;
    }

    pub(super) fn check_fanout_read_fault(
        &self,
        op: super::events::FanoutReadOp<'_>,
    ) -> Result<(), store::StoreError> {
        let hook = self.fanout_read_fault.lock().clone();
        match hook {
            Some(f) => f(op),
            None => Ok(()),
        }
    }

    pub(crate) fn list_all_plugins(
        store: &Store,
        lifecycle: Option<store::plugins::PluginLifecycle>,
    ) -> Result<Vec<PluginRecord>, store::StoreError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store.list_plugins_page(lifecycle, cursor.as_deref(), Some(200))?;
            all.extend(page.items);
            match page.next_cursor {
                Some(next) if cursor.as_deref() != Some(next.as_str()) => cursor = Some(next),
                _ => break,
            }
        }
        Ok(all)
    }

    pub(crate) fn events_notify(&self) -> &Arc<tokio::sync::Notify> {
        &self.events_notify
    }

    pub(crate) fn record_checkin_trace(&self, trace: hooks::HookTrace) {
        self.hook_traces.lock().record_checkin(trace);
    }

    pub fn take_recent_checkin_traces_for_test(&self) -> Vec<hooks::HookTrace> {
        self.hook_traces.lock().take_checkin()
    }

    pub(crate) fn record_subscribe_trace(&self, id: domain::SubscribeId, trace: hooks::HookTrace) {
        self.hook_traces.lock().record_subscribe(id, trace);
    }

    pub(crate) fn list_subscribe_traces(&self, id: domain::SubscribeId) -> Vec<hooks::HookTrace> {
        self.hook_traces.lock().list_subscribe(id)
    }

    pub fn with_store_and_jobs(store: Arc<Store>, jobs: Arc<JobsWriter>) -> Self {
        Self::new().with_effects(Arc::new(ProductionEffectAdapter::with_handles(store, jobs)))
    }

    pub fn with_effects(mut self, adapter: Arc<dyn PluginEffectAdapter>) -> Self {
        self.effects = adapter;
        self
    }

    pub fn with_task_runner(mut self, runner: Arc<dyn PluginTaskRunner>) -> Self {
        self.task_runner = runner;
        self
    }

    pub fn with_pre_effect_hook(
        mut self,
        hook: Arc<dyn Fn(PluginOperationId) + Send + Sync>,
    ) -> Self {
        self.pre_effect_hook = Some(hook);
        self
    }

    pub fn effects(&self) -> &Arc<dyn PluginEffectAdapter> {
        &self.effects
    }

    pub fn task_runner(&self) -> &Arc<dyn PluginTaskRunner> {
        &self.task_runner
    }

    pub fn invoke_endpoint(
        &self,
        state: &ApiState,
        plugin: &PluginRecord,
        authority: PluginStateAuthority,
        subject: PluginSubject,
        cause: PluginCommandCause,
        request: EndpointRequest,
    ) -> Result<EndpointResponse, Failure> {
        run_endpoint(self, state, plugin, authority, subject, cause, request)
    }

    fn runtime_group(&self) -> Result<&RuntimeGroup, Failure> {
        let init = self.runtime.get_or_init(|| {
            let runtime = PluginRuntime::new(Limits::default())?;
            let wasm_host = runtime.host(self.ports.clone());
            let validation_host = runtime.host(self.ports.clone());
            Ok(RuntimeGroup {
                runtime,
                wasm_host,
                validation_host,
            })
        });
        match init {
            Ok(group) => Ok(group),
            Err(err) => Err(err.clone()),
        }
    }

    pub fn wasm_host(&self) -> Result<&PluginHost, Failure> {
        Ok(&self.runtime_group()?.wasm_host)
    }

    pub(crate) fn validation_host(&self) -> Result<&PluginHost, Failure> {
        Ok(&self.runtime_group()?.validation_host)
    }

    pub(crate) fn maybe_runtime(&self) -> Option<&PluginRuntime> {
        self.runtime
            .get()
            .and_then(|res| res.as_ref().ok().map(|g| &g.runtime))
    }

    pub(crate) fn ports(&self) -> &Arc<StoreHostPorts> {
        &self.ports
    }

    pub fn host_ports(&self) -> Arc<dyn plugin_host::HostPorts> {
        self.ports.clone()
    }

    pub(crate) fn cache(&self) -> &ComponentCache {
        &self.cache
    }

    pub(crate) fn in_flight(&self) -> &Arc<InFlightRegistry> {
        &self.in_flight
    }

    #[doc(hidden)]
    pub fn enter_library_invocation_for_test(
        &self,
        plugin_id: &str,
        generation: i64,
        inv: &plugin_host::Invocation,
    ) -> Result<super::in_flight::OwnedInFlightGuard, plugin_api::Failure> {
        self.in_flight.try_enter_owned(plugin_id, generation, inv)
    }

    pub(crate) fn consent(&self) -> &Arc<ConsentStore> {
        &self.consent
    }

    pub(crate) fn reauth(&self) -> &Arc<ReauthService> {
        &self.reauth
    }

    pub(crate) fn cleanup_tokens(&self) -> &Arc<CleanupTokenStore> {
        &self.cleanup_tokens
    }

    pub(crate) fn staging_pins(&self) -> &Arc<ActiveStagingPins> {
        &self.staging_pins
    }

    pub(crate) fn ui_handles(&self) -> &super::ui_handles::UiHandleSigner {
        &self.ui_handles
    }

    pub(crate) fn verify_ui_handle(
        &self,
        plugin_id: &str,
        generation: i64,
        actor_user_id: domain::UserId,
        expected_kind: Option<&str>,
        token: &str,
    ) -> Result<super::ui_handles::VerifiedUiHandle, PluginServiceError> {
        let uid = actor_user_id.to_string();
        self.ui_handles
            .verify(token, plugin_id, generation, &uid, expected_kind)
    }

    pub(crate) fn pre_effect_hook(&self) -> Option<&Arc<dyn Fn(PluginOperationId) + Send + Sync>> {
        self.pre_effect_hook.as_ref()
    }

    pub(crate) fn tasks_cursor(&self) -> &Mutex<Option<uuid::Uuid>> {
        &self.tasks_cursor
    }

    pub(crate) fn commands_cursor(&self) -> &Mutex<Option<PluginCommandCursor>> {
        &self.commands_cursor
    }

    pub(crate) fn unresolved_cursor(&self) -> &Mutex<Option<uuid::Uuid>> {
        &self.unresolved_cursor
    }

    pub async fn issue_reauth_token(
        &self,
        state: &ApiState,
        user_id: domain::UserId,
        password: String,
        purpose: super::types::ReauthPurpose,
    ) -> Result<super::types::ReauthTokenView, PluginServiceError> {
        self.reauth
            .issue_reauth_token(state, user_id, password, purpose)
            .await
    }

    pub async fn peek_reauth_token(
        &self,
        state: &ApiState,
        user_id: domain::UserId,
        purpose: super::types::ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        self.reauth.peek(state, user_id, purpose, token).await
    }

    pub async fn verify_and_consume_reauth_token(
        &self,
        state: &ApiState,
        user_id: domain::UserId,
        purpose: super::types::ReauthPurpose,
        token: &str,
    ) -> Result<(), PluginServiceError> {
        self.reauth
            .verify_and_consume(state, user_id, purpose, token)
            .await
    }

    pub async fn stage_upload(
        &self,
        state: &ApiState,
        input: StageUploadInput,
    ) -> Result<StagedUploadView, PluginServiceError> {
        require_foreground_write_admitted(state)?;
        super::idempotency::validate_idempotency_input(&input.idempotency_key, &input.request)?;
        if let Some(replayed) = self.peek_upload_reauth_or_replay(state, &input).await? {
            return Ok(replayed);
        }
        let sig = input.signature_text.clone();
        let policy = run_app_read(state, move |store| {
            build_package_policy_from_store(store, sig)
        })
        .await?;
        let data_dir = state.store.data_dir().to_path_buf();
        let cleanup_dir = super::staging::uploads_root(&data_dir).join(input.upload_id.to_string());
        let actor = input.actor_user_id;
        let upload_id = input.upload_id;
        let bytes = input.cmplugin_bytes;
        let sig_text = input.signature_text;
        let exp_sha = input.expected_sha256;
        let reauth_token = input.reauth_token;
        let idem_key = input.idempotency_key;
        let req = input.request;
        let view =
            match run_blocking(move || {
                stage_upload_sync(
                    &data_dir,
                    actor,
                    upload_id,
                    &bytes,
                    sig_text.as_deref(),
                    exp_sha.as_deref(),
                    &policy,
                )
            })
            .await
            {
                Ok(v) => v,
                Err(err) => {
                    if matches!(err, PluginServiceError::Conflict { .. })
                        && let Some(replayed) =
                            super::idempotency::check_idempotent_replay_after_write::<
                                StagedUploadView,
                            >(
                                state, actor, "upload-stage", &idem_key, &req
                            )
                            .await?
                    {
                        return Ok(replayed);
                    }
                    return Err(err);
                }
            };
        let sig_status = view.signature_status.clone();
        let signing_key_id = view.signing_key_id.clone();
        let json =
            serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        let reauth_tx = self.reauth.clone();
        let idem_tx = idem_key.clone();
        let req_tx = req.clone();
        let saved = match super::config_ops::run_app_write_tx(state, move |app| {
            let subject = PluginSubject::User(actor);
            if let Some(existing) =
                app.get_plugin_idempotency_envelope(&subject, "upload-stage", &idem_tx, &req_tx)?
            {
                return Ok(existing);
            }
            verify_upload_trust_in_app(app, &sig_status, signing_key_id.as_deref())?;
            reauth_tx.consume_in_app_tx(
                app,
                actor,
                super::types::ReauthPurpose::PluginUpload,
                &reauth_token,
            )?;
            app.put_plugin_idempotency_envelope(
                &subject,
                "upload-stage",
                &idem_tx,
                &req_tx,
                &json,
            )?;
            Ok(json)
        })
        .await
        {
            Ok(saved) => saved,
            Err(err) => {
                return handle_failed_upload_reauth(
                    state,
                    actor,
                    upload_id,
                    &idem_key,
                    &req,
                    cleanup_dir,
                    err,
                )
                .await;
            }
        };
        let parsed: StagedUploadView =
            serde_json::from_value(saved).map_err(|e| PluginServiceError::Io(e.to_string()))?;
        if parsed.upload_id != upload_id {
            remove_staged_upload_dir(cleanup_dir).await;
        }
        Ok(parsed)
    }

    async fn peek_upload_reauth_or_replay(
        &self,
        state: &ApiState,
        input: &StageUploadInput,
    ) -> Result<Option<StagedUploadView>, PluginServiceError> {
        if let Some(replayed) = super::idempotency::check_idempotent_replay::<StagedUploadView>(
            state,
            input.actor_user_id,
            "upload-stage",
            &input.idempotency_key,
            &input.request,
        )
        .await?
        {
            return Ok(Some(replayed));
        }
        if let Err(err) = self
            .peek_reauth_token(
                state,
                input.actor_user_id,
                super::types::ReauthPurpose::PluginUpload,
                &input.reauth_token,
            )
            .await
        {
            if super::idempotency::is_reauth_invalid_err(&err)
                && let Some(replayed) =
                    super::idempotency::check_idempotent_replay_after_write::<StagedUploadView>(
                        state,
                        input.actor_user_id,
                        "upload-stage",
                        &input.idempotency_key,
                        &input.request,
                    )
                    .await?
            {
                return Ok(Some(replayed));
            }
            return Err(err);
        }
        Ok(None)
    }
}

fn verify_upload_trust_in_app(
    app: &store::App<'_>,
    sig_status: &str,
    signing_key_id: Option<&str>,
) -> Result<(), PluginServiceError> {
    match sig_status {
        "unsigned" | "development" => {
            if signing_key_id.is_some() {
                return Err(PluginServiceError::invalid(
                    "plugin.invalid_package",
                    "unsigned package cannot carry signing_key_id",
                ));
            }
            if !app.plugin_developer_mode()?.enabled {
                return Err(PluginServiceError::forbidden(
                    "plugin.signature_required",
                    "package signature required",
                ));
            }
            Ok(())
        }
        "signed" | "minisign" => {
            let Some(kid) = signing_key_id else {
                return Err(PluginServiceError::forbidden(
                    "plugin.untrusted_key",
                    "missing signing key id",
                ));
            };
            match app.get_plugin_trust_key(kid)? {
                Some(k) if k.status != store::plugins::PluginTrustKeyStatus::Revoked => Ok(()),
                Some(_) => Err(PluginServiceError::forbidden(
                    "plugin.key_revoked",
                    format!("signing key `{kid}` is revoked"),
                )),
                None => Err(PluginServiceError::forbidden(
                    "plugin.untrusted_key",
                    format!("signing key `{kid}` is not trusted"),
                )),
            }
        }
        _ => Err(PluginServiceError::forbidden(
            "plugin.signature_required",
            "package signature required",
        )),
    }
}

async fn handle_failed_upload_reauth(
    state: &ApiState,
    actor: domain::UserId,
    upload_id: uuid::Uuid,
    idem_key: &str,
    req: &store::plugins::CanonicalPluginRequest,
    cleanup_dir: std::path::PathBuf,
    err: PluginServiceError,
) -> Result<StagedUploadView, PluginServiceError> {
    if super::idempotency::is_reauth_invalid_err(&err) {
        match super::idempotency::check_idempotent_replay_after_write::<StagedUploadView>(
            state,
            actor,
            "upload-stage",
            idem_key,
            req,
        )
        .await
        {
            Ok(Some(replayed)) if replayed.upload_id == upload_id => return Ok(replayed),
            Ok(Some(replayed)) => {
                remove_staged_upload_dir(cleanup_dir).await;
                return Ok(replayed);
            }
            Ok(None) => {}
            Err(replay_err) => {
                remove_staged_upload_dir(cleanup_dir).await;
                return Err(replay_err);
            }
        }
    }
    remove_staged_upload_dir(cleanup_dir).await;
    Err(err)
}

async fn remove_staged_upload_dir(dir: std::path::PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(dir);
    })
    .await;
}
