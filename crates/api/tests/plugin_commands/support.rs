#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use api::plugins::{
    ExternalEffectStep, PluginEffectAdapter, PluginEffectContext, PluginInvocationRole,
    PluginServiceError, PluginTaskInvocation, PluginTaskRunner, SubmitPluginCommand,
    submit_command,
};
use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{MaintenanceVia, PluginOperationId, Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use store::plugins::*;
use tower::ServiceExt;

pub const TOKEN: &str = "plugin-commands-fixture-token";

pub struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

#[derive(Default)]
pub struct Gate {
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub fn block(&self) {
        self.entered.store(true, Ordering::Release);
        let mut released = self.released.lock();
        self.changed
            .wait_while_for(&mut released, |done| !*done, Duration::from_secs(10));
        assert!(*released, "gate must be released by test");
    }

    pub fn wait_entered(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !self.entered.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for gated execution"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn release(&self) {
        *self.released.lock() = true;
        self.changed.notify_all();
    }
}

pub enum FakeEffectMode {
    Succeed,
    UnknownThenReconcile,
    Reject(String),
    Blocked(Arc<Gate>),
}

pub struct FakeEffects {
    pub supported_kinds: Vec<String>,
    pub mode: Mutex<FakeEffectMode>,
    pub reconcile_outcome: Mutex<Option<PluginReconciliationOutcome>>,
    pub executed: Mutex<Vec<PluginOperationId>>,
    pub receipts: Mutex<HashMap<PluginOperationId, Vec<u8>>>,
}

impl FakeEffects {
    pub fn new(mode: FakeEffectMode) -> Self {
        Self {
            supported_kinds: vec!["fixture".into()],
            mode: Mutex::new(mode),
            reconcile_outcome: Mutex::new(None),
            executed: Mutex::new(Vec::new()),
            receipts: Mutex::new(HashMap::new()),
        }
    }

    pub fn calls(&self) -> usize {
        self.executed.lock().len()
    }
}

impl PluginEffectAdapter for FakeEffects {
    fn supports_kind(&self, kind: &str) -> bool {
        self.supported_kinds.iter().any(|k| k == kind)
    }

    fn execute(&self, ctx: &PluginEffectContext<'_>) -> ExternalEffectStep {
        self.executed.lock().push(ctx.operation_id);
        let gate = {
            let mode = self.mode.lock();
            match &*mode {
                FakeEffectMode::Succeed => {
                    let result = format!("receipt:{}", ctx.operation_id).into_bytes();
                    self.receipts
                        .lock()
                        .insert(ctx.operation_id, result.clone());
                    return ExternalEffectStep::Succeeded {
                        result: Some(result),
                    };
                }
                FakeEffectMode::UnknownThenReconcile => {
                    self.receipts
                        .lock()
                        .insert(ctx.operation_id, b"external-recorded".to_vec());
                    return ExternalEffectStep::Unknown {
                        phase: "reply-lost".into(),
                    };
                }
                FakeEffectMode::Reject(phase) => {
                    return ExternalEffectStep::Rejected {
                        phase: phase.clone(),
                    };
                }
                FakeEffectMode::Blocked(gate) => Arc::clone(gate),
            }
        };
        gate.block();
        ExternalEffectStep::Succeeded {
            result: Some(format!("unblocked:{}", ctx.operation_id).into_bytes()),
        }
    }

    fn reconcile(
        &self,
        operation_id: PluginOperationId,
        _kind: &str,
        _attempt: i64,
    ) -> Option<PluginReconciliationOutcome> {
        if !self.receipts.lock().contains_key(&operation_id) {
            return None;
        }
        *self.reconcile_outcome.lock()
    }
}

#[derive(Default)]
pub struct FakeTaskRunner {
    pub seen: Mutex<Vec<(String, Value)>>,
    pub count: AtomicUsize,
}

impl PluginTaskRunner for FakeTaskRunner {
    fn run_task(&self, _state: &ApiState, invocation: &PluginTaskInvocation) -> Result<(), String> {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.seen.lock().push((
            invocation.binding.task_id.clone(),
            invocation.binding.definition.public_config.clone(),
        ));
        Ok(())
    }
}

pub fn build_state(
    dir: &std::path::Path,
    effects: Arc<FakeEffects>,
    tasks: Option<Arc<FakeTaskRunner>>,
) -> ApiState {
    let store = Store::open(dir.join("data")).unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.join("stage")));
    let mut state = ApiState::new(
        store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        dl,
    )
    .unwrap()
    .with_plugin_effects(effects);
    if let Some(tasks) = tasks {
        state = state.with_plugin_task_runner(tasks);
    }
    {
        let jobs = state.jobs();
        let queue = jobs.acquire().unwrap();
        for def in queue.list_defs().unwrap() {
            if !def.kind.is_plugin() {
                let _ = queue.set_def_enabled_by_id(def.id, false);
            }
        }
    }
    state
}

pub fn op_state(state: &ApiState, subject: &PluginSubject, id: uuid::Uuid) -> PluginOperation {
    state
        .store()
        .get_plugin_operation(subject, id)
        .unwrap()
        .unwrap()
}

pub fn task_fence(state: &ApiState, binding_id: uuid::Uuid) -> Option<jobs::PluginTaskFence> {
    state
        .jobs()
        .acquire()
        .unwrap()
        .plugin_task_fence(domain::PluginTaskBindingId::from_uuid(binding_id))
        .unwrap()
}

pub fn seed_pre_crash_state(dir: &std::path::Path) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let store = Store::open(dir.join("data")).unwrap();
    let p = install_and_enable(&store, "org.example.crash", &[]);
    let auth = store.issue_plugin_state_authority(&p, None).unwrap();
    let cmd_auth = store
        .authorize_plugin_command(&auth, "fixture", &[PluginCommandTarget::Global])
        .unwrap();
    let subject = PluginSubject::User(domain::UserId::new());
    let mk_req = |key: &str| AcceptPluginOperation {
        plugin_id: Some(p.plugin_id.clone()),
        subject: subject.clone(),
        idempotency_key: key.into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/plugins/fixture/run".into(),
            body_digest: [1; 32],
        },
        kind: "fixture".into(),
        expected_revision: Some(p.revision),
        expected_generation: Some(p.generation),
        cause: PluginCommandCause::root(format!("cause-{key}")),
    };
    let queued = store
        .accept_plugin_command(&cmd_auth, &mk_req("queued-crash"), b"")
        .unwrap()
        .operation;
    let running = store
        .accept_plugin_command(&cmd_auth, &mk_req("running-crash"), b"")
        .unwrap()
        .operation;
    store
        .claim_plugin_command(
            running.operation_id,
            running.revision,
            running.generation,
            0,
            5,
        )
        .unwrap();
    let task = store
        .save_plugin_task_binding(
            &p,
            "crash-task",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(60),
                public_config: serde_json::json!({"ok": true}),
            },
        )
        .unwrap();
    (queued.operation_id, running.operation_id, task.binding_id)
}

pub fn site() -> Site {
    Site {
        id: SiteId::new(),
        name: "Fixture".into(),
        url: "https://fixture.invalid/".into(),
        profile_id: "fixture".into(),
        cookie: Some("cookie".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

pub fn install_and_enable(store: &Store, id: &str, sites: &[Site]) -> PluginRecord {
    let meta = PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: "pkg-hash-v1".into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: "/unused/pkg".into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: !sites.is_empty(),
    };
    let installed = store.register_plugin_version(&meta, 0).unwrap();
    let scopes: Vec<_> = if sites.is_empty() {
        vec![PluginGrantScope::Global]
    } else {
        sites.iter().map(|s| PluginGrantScope::Site(s.id)).collect()
    };
    let grants: Vec<_> = scopes
        .into_iter()
        .map(|scope| PluginGrant {
            capability: "command:fixture".into(),
            scope,
            sensitive: false,
        })
        .collect();
    let p = store
        .replace_plugin_grants(id, &installed.package_hash, installed.revision, &grants)
        .unwrap();
    enable(store, &p)
}

pub fn enable(store: &Store, p: &PluginRecord) -> PluginRecord {
    let p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: p.plugin_id.clone(),
            operation_id: p.active_operation_id.unwrap(),
            revision: p.revision,
            generation: p.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

pub fn disable(store: &Store, p: &PluginRecord) -> PluginRecord {
    let p = store
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: p.plugin_id.clone(),
            operation_id: p.active_operation_id.unwrap(),
            revision: p.revision,
            generation: p.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

pub fn submit(
    state: &ApiState,
    role: PluginInvocationRole,
    authority: PluginStateAuthority,
    subject: PluginSubject,
    key: &str,
    cause: PluginCommandCause,
    site_id: Option<SiteId>,
) -> Result<AcceptedPluginOperation, PluginServiceError> {
    let target = site_id.map_or(PluginCommandTarget::Global, PluginCommandTarget::Site);
    submit_command(
        state,
        SubmitPluginCommand {
            role,
            authority,
            subject,
            kind: "fixture".into(),
            targets: vec![target],
            idempotency_key: key.into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/fixture/run".into(),
                body_digest: [7; 32],
            },
            expected_revision: None,
            cause,
            payload: b"cmd-body".to_vec(),
        },
    )
}

pub fn tick(state: &ApiState, now: i64) -> Value {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let resp = api::router(state.clone())
            .oneshot(
                Request::post(format!("/api/v1/jobs/tick?now={now}"))
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    })
}
