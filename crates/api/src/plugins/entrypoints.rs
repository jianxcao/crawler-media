use super::events::PluginEventDispatcherHandle;
use super::service::SubmitPluginCommand;
use crate::management::ApiState;

#[doc(hidden)]
pub fn recover_plugin_startup_for_test(state: &ApiState) -> Result<(), String> {
    super::recover_startup(state).map_err(|e| e.to_string())
}

#[doc(hidden)]
pub async fn drain_plugin_events_for_test(state: &ApiState, now_ms: i64) {
    super::events::drain_once(state, now_ms).await;
}

#[doc(hidden)]
pub fn recover_plugin_events_startup_for_test(state: &ApiState, now_ms: i64) {
    super::events::recover_startup(state, now_ms);
}

pub fn spawn_plugin_event_dispatcher(state: ApiState) {
    super::events::spawn_dispatcher(state);
}

pub fn spawn_plugin_event_dispatcher_with_handle(state: ApiState) -> PluginEventDispatcherHandle {
    super::events::spawn_dispatcher_with_handle(state)
}

#[doc(hidden)]
pub fn reconcile_plugin_pending_for_test(state: &ApiState) -> Result<(), String> {
    super::reconcile_pending(state, crate::job_loop::unix_now()).map_err(|e| e.to_string())
}

#[doc(hidden)]
pub fn submit_plugin_command_for_test(
    state: &ApiState,
    req: SubmitPluginCommand,
) -> Result<store::plugins::AcceptedPluginOperation, String> {
    super::submit_command(state, req).map_err(|e| e.to_string())
}

#[doc(hidden)]
pub fn run_plugin_command_for_test(
    state: &ApiState,
    op_id: domain::PluginOperationId,
) -> Result<(), String> {
    let job = jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginCommand,
        scope: jobs::JobScope::PluginOperation(op_id),
        status: jobs::JobStatus::Running,
        attempt: 1,
        payload: String::new(),
        run_after: 0,
        started_at: Some(crate::job_loop::unix_now()),
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: None,
        timeout_secs: None,
    };
    super::run_command_job(state, &job)
}

#[doc(hidden)]
pub fn build_hook_pipeline_for_test(state: &ApiState, hook_id: &str) -> hooks::HookPipeline {
    super::hooks_runtime::build_hook_pipeline(state, hook_id)
}

#[doc(hidden)]
pub fn record_subscribe_trace_for_test(
    state: &ApiState,
    id: domain::SubscribeId,
    trace: hooks::HookTrace,
) {
    state.plugins.record_subscribe_trace(id, trace);
}

#[doc(hidden)]
pub fn run_plugin_reconcile_for_test(
    state: &ApiState,
    op_id: domain::PluginOperationId,
) -> Result<(), String> {
    let job = jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginReconcile,
        scope: jobs::JobScope::PluginOperation(op_id),
        status: jobs::JobStatus::Running,
        attempt: 1,
        payload: String::new(),
        run_after: 0,
        started_at: Some(crate::job_loop::unix_now()),
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: None,
        timeout_secs: None,
    };
    super::run_reconcile_job(state, &job)
}
