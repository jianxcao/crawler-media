use plugin_api::{
    ApiError, Candidate, CommandAction, CommandRequest, ConfigView, EndpointRequest, EntryPage,
    ErrorCode, Failure, GuestRole, HealthState, LogLevel, OperationPage, OperationState,
    OperationView, PageRequest, SearchAfter, StateEntry, TaskInput,
};
use plugin_host::{
    ENDPOINT_WORLD, HOOK_WORLD, HostPorts, Invocation, KvValue, Limits, PluginRuntime, TASK_WORLD,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Duration,
};

fn policy_bytes() -> &'static [u8] {
    include_bytes!("fixtures/policy.wasm")
}

fn task_bytes() -> &'static [u8] {
    include_bytes!("fixtures/task_fixture.wasm")
}

fn endpoint_bytes() -> &'static [u8] {
    include_bytes!("fixtures/endpoint_fixture.wasm")
}

fn api_err(code: ErrorCode, message: &str) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

type LogRecord = (LogLevel, String, Vec<(String, String)>);
type SubmissionRecord = (GuestRole, String, Option<String>, CommandRequest);

#[derive(Default)]
struct MemoryPorts {
    kv: Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
    blob: Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
    ops: Mutex<BTreeMap<String, OperationView>>,
    logs: Mutex<Vec<LogRecord>>,
    submissions: Mutex<Vec<SubmissionRecord>>,
    next_op: Mutex<u64>,
}

impl MemoryPorts {
    fn put_map(
        map: &Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        let mut guard = map.lock().unwrap();
        let current = guard.get(key).map(|(_, rev)| *rev).unwrap_or(0);
        if let Some(tag) = if_match
            && tag != current.to_string()
        {
            return Err(api_err(ErrorCode::Conflict, "etag mismatch"));
        }
        let next = current + 1;
        guard.insert(key.into(), (value.to_vec(), next));
        Ok(next.to_string())
    }

    fn delete_map(
        map: &Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
        key: &str,
        if_match: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut guard = map.lock().unwrap();
        let Some((_, rev)) = guard.get(key) else {
            return Err(api_err(ErrorCode::NotFound, "missing key"));
        };
        if let Some(tag) = if_match
            && tag != rev.to_string()
        {
            return Err(api_err(ErrorCode::Conflict, "etag mismatch"));
        }
        guard.remove(key);
        Ok(())
    }

    fn list_map(
        map: &Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
        prefix: &str,
        page: PageRequest,
    ) -> EntryPage {
        let guard = map.lock().unwrap();
        let limit = (page.limit.max(1) as usize).min(100);
        let mut items: Vec<StateEntry> = guard
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .filter(|(k, _)| page.cursor.as_ref().is_none_or(|c| k.as_str() > c.as_str()))
            .take(limit)
            .map(|(k, (v, rev))| StateEntry {
                key: k.clone(),
                value: v.clone(),
                etag: rev.to_string(),
            })
            .collect();
        let next_cursor = if items.len() == limit {
            items.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        items.truncate(limit);
        EntryPage { items, next_cursor }
    }
}

impl HostPorts for MemoryPorts {
    fn authorized(&self, _: &Invocation, _: &str) -> bool {
        true
    }

    fn config_get(&self, _: &Invocation) -> Result<ConfigView, ApiError> {
        Ok(ConfigView {
            revision: 3,
            validated_json: "{\"mode\":\"ok\"}".into(),
        })
    }

    fn log_write(
        &self,
        _: &Invocation,
        level: LogLevel,
        message: &str,
        fields: &[(String, String)],
    ) -> Result<(), ApiError> {
        self.logs
            .lock()
            .unwrap()
            .push((level, message.into(), fields.to_vec()));
        Ok(())
    }

    fn kv_get(&self, _: &Invocation, key: &str) -> Result<Option<KvValue>, ApiError> {
        Ok(self.kv.lock().unwrap().get(key).map(|(v, rev)| KvValue {
            value: v.clone(),
            etag: rev.to_string(),
        }))
    }

    fn kv_list(
        &self,
        _: &Invocation,
        prefix: &str,
        page: PageRequest,
    ) -> Result<EntryPage, ApiError> {
        Ok(Self::list_map(&self.kv, prefix, page))
    }

    fn kv_put(
        &self,
        _: &Invocation,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        Self::put_map(&self.kv, key, value, if_match)
    }

    fn kv_delete(&self, _: &Invocation, key: &str, if_match: Option<&str>) -> Result<(), ApiError> {
        Self::delete_map(&self.kv, key, if_match)
    }

    fn blob_get(&self, _: &Invocation, key: &str) -> Result<StateEntry, ApiError> {
        self.blob
            .lock()
            .unwrap()
            .get(key)
            .map(|(v, rev)| StateEntry {
                key: key.into(),
                value: v.clone(),
                etag: rev.to_string(),
            })
            .ok_or_else(|| api_err(ErrorCode::NotFound, "missing blob"))
    }

    fn blob_list(
        &self,
        _: &Invocation,
        prefix: &str,
        page: PageRequest,
    ) -> Result<EntryPage, ApiError> {
        Ok(Self::list_map(&self.blob, prefix, page))
    }

    fn blob_put(
        &self,
        _: &Invocation,
        key: &str,
        value: &[u8],
        if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        Self::put_map(&self.blob, key, value, if_match)
    }

    fn blob_delete(
        &self,
        _: &Invocation,
        key: &str,
        if_match: Option<&str>,
    ) -> Result<(), ApiError> {
        Self::delete_map(&self.blob, key, if_match)
    }

    fn operations_get(&self, _: &Invocation, id: &str) -> Result<OperationView, ApiError> {
        self.ops
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| api_err(ErrorCode::NotFound, "missing op"))
    }

    fn operations_list(
        &self,
        _: &Invocation,
        page: PageRequest,
    ) -> Result<OperationPage, ApiError> {
        let items: Vec<_> = self
            .ops
            .lock()
            .unwrap()
            .values()
            .filter(|op| page.cursor.as_ref().is_none_or(|c| op.id > *c))
            .take(page.limit.max(1) as usize)
            .cloned()
            .collect();
        Ok(OperationPage {
            items,
            next_cursor: None,
        })
    }

    fn commands_submit(
        &self,
        inv: &Invocation,
        request: CommandRequest,
    ) -> Result<String, ApiError> {
        let mut seq = self.next_op.lock().unwrap();
        *seq += 1;
        let id = format!("op-{:04}", *seq);
        self.ops.lock().unwrap().insert(
            id.clone(),
            OperationView {
                id: id.clone(),
                state: OperationState::Queued,
                revision: 1,
                next_run_at: Some(0),
            },
        );
        self.submissions.lock().unwrap().push((
            inv.role,
            inv.cause_id.clone(),
            inv.site_id.clone(),
            request,
        ));
        Ok(id)
    }
}

struct BlockingKvPort {
    entered: mpsc::Sender<String>,
    release: (Mutex<HashMap<String, bool>>, Condvar),
}

impl BlockingKvPort {
    fn new(entered: mpsc::Sender<String>) -> Self {
        Self {
            entered,
            release: (Mutex::new(HashMap::new()), Condvar::new()),
        }
    }

    fn release(&self, plugin: &str) {
        self.release.0.lock().unwrap().insert(plugin.into(), true);
        self.release.1.notify_all();
    }
}

impl HostPorts for BlockingKvPort {
    fn authorized(&self, _: &Invocation, _: &str) -> bool {
        true
    }

    fn kv_get(&self, inv: &Invocation, _: &str) -> Result<Option<KvValue>, ApiError> {
        if inv.plugin_id == "bg-1" {
            let _ = self.entered.send(inv.plugin_id.clone());
            let mut guard = self.release.0.lock().unwrap();
            while !guard.get(&inv.plugin_id).copied().unwrap_or(false) {
                guard = self.release.1.wait(guard).unwrap();
            }
        }
        Ok(None)
    }
}

fn candidate(title: &str) -> Candidate {
    Candidate {
        handle: format!("cand-{title}"),
        site_id: "site-1".into(),
        provider: "builtin".into(),
        generation: 1,
        title: title.into(),
        size: Some(10),
        seeders: Some(1),
        leechers: None,
        published_at: None,
        free: None,
        hr: None,
        release: None,
        score: None,
        labels: Vec::new(),
        revision: 1,
    }
}

#[test]
fn background_saturation_preserves_interactive_reserve_for_hooks_and_endpoints() {
    let (tx, rx) = mpsc::channel();
    let ports = Arc::new(BlockingKvPort::new(tx));
    let runtime = PluginRuntime::new(Limits {
        workers: 2,
        background_workers: 2,
        global_active: 2,
        interactive_reserve: 1,
        per_plugin: 1,
        ..Limits::default()
    })
    .unwrap();
    let host = runtime.host(ports.clone());
    let policy = host.load(policy_bytes(), HOOK_WORLD).unwrap();
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();
    let endpoint = host
        .load_endpoint(endpoint_bytes(), ENDPOINT_WORLD)
        .unwrap();

    let (h1, t1) = (host.clone(), task.clone());
    let bg1 = thread::spawn(move || {
        h1.invoke_task(
            &t1,
            Invocation::new("bg-1", Duration::from_secs(2)),
            TaskInput {
                task_id: "block".into(),
                validated_input: "block-kv".into(),
                job_id: "job-1".into(),
                attempt: 1,
            },
        )
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "bg-1");

    let bg2 = host.invoke_task(
        &task,
        Invocation::new("bg-2", Duration::from_millis(100)),
        TaskInput {
            task_id: "bg-2".into(),
            validated_input: "block-kv".into(),
            job_id: "job-2".into(),
            attempt: 1,
        },
    );
    assert_eq!(bg2, Err(Failure::Busy));

    let hook = host.invoke(
        &policy,
        Invocation::new("hook-1", Duration::from_secs(1)),
        SearchAfter {
            candidates: vec![candidate("B")],
            site_failures: Vec::new(),
        },
    );
    assert!(hook.failure.is_none(), "{:?}", hook.failure);

    let ep = host.invoke_endpoint(
        &endpoint,
        Invocation::new("ep-1", Duration::from_secs(1)),
        EndpointRequest {
            method: "GET".into(),
            path: "/kv-get".into(),
            query: Vec::new(),
            body: Vec::new(),
        },
    );
    assert_eq!(ep.unwrap().status, 404);

    ports.release("bg-1");
    let outcome = bg1.join().unwrap().unwrap();
    assert_eq!(outcome.summary, "blocked-done");
}

#[test]
fn task_component_exercises_scoped_imports_and_lifecycle() {
    let ports = Arc::new(MemoryPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    assert!(matches!(
        host.load_task(task_bytes(), HOOK_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();
    assert!(
        host.validate_task(
            &task,
            Invocation::new("task-p", Duration::from_secs(1)),
            "{}",
            1
        )
        .is_ok()
    );
    assert_eq!(
        host.validate_task(
            &task,
            Invocation::new("task-p", Duration::from_secs(1)),
            "reject",
            1
        )
        .unwrap_err()
        .code_or_panic(),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        host.health_task(&task, Invocation::new("task-p", Duration::from_secs(1)))
            .unwrap(),
        HealthState::Healthy
    );

    let mut inv_t = Invocation::new("task-p", Duration::from_secs(2));
    inv_t.cause_id = "cause-task-42".into();
    let outcome = host
        .invoke_task(
            &task,
            inv_t,
            TaskInput {
                task_id: "sync".into(),
                validated_input: "submit-and-state".into(),
                job_id: "job-9".into(),
                attempt: 1,
            },
        )
        .unwrap();
    assert_eq!(outcome.summary, "{\"mode\":\"ok\"}");
    assert_eq!(outcome.operation_ids, vec!["op-0001".to_string()]);
    let subs = ports.submissions.lock().unwrap();
    assert_eq!(subs[0].0, GuestRole::Task);
    assert_eq!(subs[0].1, "cause-task-42");
    assert_eq!(
        subs[0].3.action,
        CommandAction::SiteMaintenance {
            site_handle: "global".into(),
            action: plugin_api::MaintenanceAction::CheckIn,
        }
    );
    assert_eq!(ports.logs.lock().unwrap().len(), 1);
}

#[test]
fn endpoint_component_exercises_kv_and_command_submit() {
    let ports = Arc::new(MemoryPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    assert!(matches!(
        host.load_endpoint(endpoint_bytes(), TASK_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    let endpoint = host
        .load_endpoint(endpoint_bytes(), ENDPOINT_WORLD)
        .unwrap();

    let mut inv_e = Invocation::new("ep-p", Duration::from_secs(2));
    inv_e.cause_id = "cause-ep-99".into();
    inv_e.site_id = Some("site-alpha".into());
    let put_res = host
        .invoke_endpoint(
            &endpoint,
            inv_e.clone(),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::new(),
                body: b"ep-val".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(put_res.status, 200);
    let get_res = host
        .invoke_endpoint(
            &endpoint,
            inv_e.clone(),
            EndpointRequest {
                method: "GET".into(),
                path: "/kv-get".into(),
                query: Vec::new(),
                body: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(get_res.status, 200);
    assert_eq!(get_res.body, b"ep-val");

    let sub_res = host
        .invoke_endpoint(
            &endpoint,
            inv_e,
            EndpointRequest {
                method: "POST".into(),
                path: "/submit".into(),
                query: Vec::new(),
                body: b"ep-idem-1".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(sub_res.status, 202);
    assert_eq!(sub_res.body, b"op-0001");
    let subs = ports.submissions.lock().unwrap();
    assert_eq!(subs[0].0, GuestRole::Endpoint);
    assert_eq!(subs[0].1, "cause-ep-99");
    assert_eq!(subs[0].2.as_deref(), Some("site-alpha"));
}

#[test]
fn commands_submit_outside_task_or_endpoint_returns_wrong_context() {
    let ports = Arc::new(MemoryPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports);
    assert!(matches!(
        host.load(task_bytes(), HOOK_WORLD),
        Err(Failure::InvalidComponent(_))
    ));
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();
    let err = host
        .validate_task(
            &task,
            Invocation::new("task-p", Duration::from_secs(1)),
            "wrong-context-submit",
            1,
        )
        .unwrap_err();
    assert!(
        matches!(err, Failure::Guest(ref api) if api.code == ErrorCode::WrongContext),
        "expected WrongContext, got {err:?}"
    );
}

#[test]
fn dropping_runtime_and_hosts_stops_workers_cleanly() {
    let ports = Arc::new(MemoryPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports);
    let policy = host.load(policy_bytes(), HOOK_WORLD).unwrap();
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();
    let hook = host.invoke(
        &policy,
        Invocation::new("p1", Duration::from_secs(1)),
        SearchAfter {
            candidates: vec![candidate("B")],
            site_failures: Vec::new(),
        },
    );
    assert!(hook.failure.is_none());
    let out = host
        .invoke_task(
            &task,
            Invocation::new("p2", Duration::from_secs(1)),
            TaskInput {
                task_id: "t".into(),
                validated_input: "submit-and-state".into(),
                job_id: "j1".into(),
                attempt: 1,
            },
        )
        .unwrap();
    assert_eq!(out.progress, 100);
    drop(host);
    drop(runtime);
}

#[test]
fn lifecycle_validate_rejects_kv_and_blob_writes_with_wrong_context() {
    let ports = Arc::new(MemoryPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports);
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();
    for mode in [
        "wrong-context-kv-put",
        "wrong-context-kv-delete",
        "wrong-context-blob-put",
        "wrong-context-blob-delete",
    ] {
        let err = host
            .validate_task(
                &task,
                Invocation::new("task-p", Duration::from_secs(1)),
                mode,
                1,
            )
            .unwrap_err();
        assert!(
            matches!(err, Failure::Guest(ref api) if api.code == ErrorCode::WrongContext),
            "mode {mode}: expected WrongContext, got {err:?}"
        );
    }
}

#[test]
fn wait_plugin_drained_observes_actual_worker_exit_after_timeout() {
    let (tx, rx) = mpsc::channel();
    let ports = Arc::new(BlockingKvPort::new(tx));
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    let task = host.load_task(task_bytes(), TASK_WORLD).unwrap();

    let (h1, t1) = (host.clone(), task.clone());
    let handle = thread::spawn(move || {
        h1.invoke_task(
            &t1,
            Invocation::new("bg-1", Duration::from_millis(40)),
            TaskInput {
                task_id: "block".into(),
                validated_input: "block-kv".into(),
                job_id: "job-drain".into(),
                attempt: 1,
            },
        )
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "bg-1");
    let res = handle.join().unwrap();
    assert_eq!(res, Err(Failure::DeadlineExceeded));

    assert_eq!(host.active_plugin_count("bg-1"), 1);
    assert_eq!(runtime.active_plugin_count("bg-1"), 1);
    assert!(!host.wait_plugin_drained("bg-1", Duration::from_millis(30)));

    ports.release("bg-1");
    assert!(host.wait_plugin_drained("bg-1", Duration::from_secs(2)));
    assert_eq!(host.active_plugin_count("bg-1"), 0);
    assert_eq!(runtime.active_plugin_count("bg-1"), 0);
}

trait FailureCodeExt {
    fn code_or_panic(&self) -> ErrorCode;
}
impl FailureCodeExt for Failure {
    fn code_or_panic(&self) -> ErrorCode {
        match self {
            Failure::Guest(err) => err.code.clone(),
            other => panic!("expected guest failure, got {other:?}"),
        }
    }
}
