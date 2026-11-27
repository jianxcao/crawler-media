use std::sync::{Arc, Mutex};
use std::time::Duration;

use hooks::{Gate, HookPipeline, Registration};
use plugin_api::{
    ApiError, BoostDisk, BoostReservationView, BoostSnapshotView, BoostTaskView, Candidate,
    CommandAction, CommandRequest, DownloadAfterEvent, DownloadBefore, DownloadOptions,
    DownloadPlanView, ENDPOINT_WORLD, EndpointRequest, ErrorCode, Event, EventPayload, FilterAfter,
    FilterInput, FilterPayload, GateInput, GatePayload, HOOK_WORLD, HookContext, OwnedRevision,
    Patch, SearchBefore, SearchQuery, SiteCheckinAfterEvent, SubscribeScore, SubscribeSelect,
    TASK_WORLD, TaskControl, TaskInput, TransferCompletedEvent, TransferRename,
};
use plugin_host::{HostPorts, Invocation, Limits, PluginHost, PluginRuntime, PolicyInvoker};

const POLICY_WASM: &[u8] = include_bytes!("fixtures/policy.wasm");
const TASK_WASM: &[u8] = include_bytes!("fixtures/task_fixture.wasm");
const ENDPOINT_WASM: &[u8] = include_bytes!("fixtures/endpoint_fixture.wasm");

fn ctx(plugin_id: &str) -> HookContext {
    HookContext {
        invocation_id: "inv-host-1".into(),
        cause_id: "cause-host-1".into(),
        plugin_id: plugin_id.into(),
        generation: 1,
        config_revision: 1,
        subject: "subject:1".into(),
        site_id: Some("site-1".into()),
        subscribe_id: Some("sub-1".into()),
        library_id: None,
        deadline_ms: 9_999_999_999,
        cancelled: false,
    }
}

fn candidate(handle: &str, title: &str) -> Candidate {
    Candidate {
        handle: handle.into(),
        site_id: "site-1".into(),
        provider: "prov-1".into(),
        generation: 1,
        title: title.into(),
        size: Some(1024),
        seeders: Some(10),
        leechers: Some(1),
        published_at: Some(100),
        free: Some(true),
        hr: Some(false),
        release: Some("1080p".into()),
        score: Some(50),
        labels: vec![],
        revision: 1,
    }
}

struct BoostAndCmdPorts {
    allow_boost: bool,
    captured_commands: Mutex<Vec<CommandRequest>>,
}

impl HostPorts for BoostAndCmdPorts {
    fn authorized(&self, _inv: &Invocation, cap: &str) -> bool {
        if cap == "read:boost" {
            return self.allow_boost;
        }
        true
    }

    fn boost_snapshot(
        &self,
        _inv: &Invocation,
        sites: &[String],
        downloaders: &[String],
    ) -> Result<BoostSnapshotView, ApiError> {
        Ok(BoostSnapshotView {
            revision: 7,
            used_bytes: 2_000,
            reserved_bytes: 500,
            retained_file_bytes: 1_000,
            reservations: vec![BoostReservationView {
                handle: "res-1".into(),
                operation_id: "op-1".into(),
                quota_key: sites.first().cloned().unwrap_or_else(|| "pool:1".into()),
                downloader_id: downloaders
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "dl-1".into()),
                site_id: Some("site-1".into()),
                info_hash: Some("hash-1".into()),
                reserved_bytes: 500,
                stage: "final".into(),
                expires_at: None,
                revision: 1,
            }],
            tasks: vec![BoostTaskView {
                handle: "tf-1".into(),
                quota_key: "pool:1".into(),
                downloader_id: "dl-1".into(),
                site_id: Some("site-1".into()),
                info_hash: "hash-2".into(),
                state: "seeding".into(),
                task_bytes: 2_000,
                retained_bytes: 0,
                uploaded_bytes: 4_000,
                hr_protected: Some(false),
                hold_until: Some(100),
                free_until: Some(900),
                protection_state: "eligible".into(),
                transfer_active: false,
                library_referenced: false,
                observed_at: 200,
                revision: 2,
            }],
            disk: BoostDisk {
                available_bytes: Some(50_000),
                observed_at: 200,
                status: "ok".into(),
            },
        })
    }

    fn commands_submit(&self, _inv: &Invocation, req: CommandRequest) -> Result<String, ApiError> {
        let mut guard = self.captured_commands.lock().unwrap();
        let idx = guard.len() + 1;
        guard.push(req);
        Ok(format!("op-dl-{idx}"))
    }
}

fn run_filter_patch(
    host: &PluginHost,
    policy: &plugin_host::CompiledPolicy,
    payload: FilterPayload,
) -> Vec<Patch> {
    host.invoke_filter(
        policy,
        Invocation::new("org.example.policy", Duration::from_secs(2)),
        FilterInput {
            schema_version: 1,
            context: ctx("org.example.policy"),
            payload,
        },
    )
    .unwrap()
}

#[test]
fn real_policy_wasm_executes_typed_filter_patches() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host: PluginHost = runtime.host(Arc::new(BoostAndCmdPorts {
        allow_boost: true,
        captured_commands: Mutex::new(Vec::new()),
    }));
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    let sb = run_filter_patch(
        &host,
        &policy,
        FilterPayload::SearchBefore(SearchBefore {
            query: SearchQuery {
                keyword: "Dune".into(),
                page: 1,
                categories: vec![],
            },
            site_ids: vec!["site-1".into()],
        }),
    );
    assert!(matches!(sb.as_slice(), [Patch::SearchQuery(_)]));

    let fa = run_filter_patch(
        &host,
        &policy,
        FilterPayload::FilterAfter(FilterAfter {
            candidates: vec![candidate("c1", "Item")],
            admitted_handles: vec!["c1".into()],
        }),
    );
    assert!(matches!(fa.as_slice(), [Patch::Rescore(_)]));

    let sc = run_filter_patch(
        &host,
        &policy,
        FilterPayload::SubscribeScore(SubscribeScore {
            subscribe_id: "sub-1".into(),
            media_id: "m-1".into(),
            candidates: vec![candidate("c1", "Item")],
        }),
    );
    assert!(matches!(sc.as_slice(), [Patch::Rescore(_)]));

    let ss = run_filter_patch(
        &host,
        &policy,
        FilterPayload::SubscribeSelect(SubscribeSelect {
            subscribe_id: "sub-1".into(),
            media_id: "m-1".into(),
            candidates: vec![candidate("c1", "Item")],
            admitted_handles: vec!["c1".into()],
        }),
    );
    assert!(matches!(ss.as_slice(), [Patch::SelectHandles(_)]));

    let db = run_filter_patch(
        &host,
        &policy,
        FilterPayload::DownloadBefore(Box::new(DownloadBefore {
            candidate: candidate("c1", "Item"),
            purpose: "subscribe".into(),
            plan: DownloadPlanView {
                downloader_id: "dl-1".into(),
                relative_directory: None,
                category: None,
                tags: vec![],
                paused: false,
                upload_limit: None,
                download_limit: None,
                sequential: false,
                first_last_piece: false,
            },
        })),
    );
    assert!(matches!(db.as_slice(), [Patch::DownloadPlan(_)]));

    let tr = run_filter_patch(
        &host,
        &policy,
        FilterPayload::TransferRename(TransferRename {
            media_id: "m-1".into(),
            library_id: "lib-1".into(),
            source_path: "/dl/item.mkv".into(),
            proposed_relative_path: "Movie/item.mkv".into(),
            is_movie: true,
        }),
    );
    assert_eq!(tr, vec![Patch::RenamePath("Movie/Renamed.mkv".into())]);
}

#[test]
fn real_policy_wasm_executes_typed_gate_and_notify_contracts() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host: PluginHost = runtime.host(Arc::new(BoostAndCmdPorts {
        allow_boost: true,
        captured_commands: Mutex::new(Vec::new()),
    }));
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    // Typed Gate via PolicyInvoker + HookPipeline
    let pipeline = HookPipeline::new();
    let host_clone = host.clone();
    let policy_clone = policy.clone();
    pipeline.register(Registration::for_hook(
        "download.before",
        10,
        "org.example.policy",
        "policy",
        Arc::new(PolicyInvoker::new(host_clone, policy_clone, || {
            Invocation::new("org.example.policy", Duration::from_secs(2))
        })),
    ));

    let defer = pipeline
        .gate_typed(&GateInput {
            schema_version: 1,
            context: ctx("org.example.policy"),
            hook_id: "download.before".into(),
            handles: vec!["defer-me".into()],
            revision: 1,
            payload: GatePayload::None,
        })
        .unwrap();
    assert_eq!(
        defer,
        Gate::Defer {
            until_ms: 9_000,
            reason: "wasm-defer".into(),
        }
    );

    // Typed Notify for download.after, transfer.completed, site.checkin.after
    for (kind, payload) in [
        (
            "download.after",
            EventPayload::DownloadAfter(DownloadAfterEvent {
                operation_id: "op-1".into(),
                downloader_id: Some("dl-1".into()),
                info_hash: Some("h1".into()),
                outcome: "submitted".into(),
                verified_total_bytes: Some(4096),
            }),
        ),
        (
            "transfer.completed",
            EventPayload::TransferCompleted(TransferCompletedEvent {
                media_id: "m-1".into(),
                library_id: "lib-1".into(),
                relative_path: "Movie/item.mkv".into(),
                file_count: 1,
                total_bytes: 4096,
            }),
        ),
        (
            "site.checkin.after",
            EventPayload::SiteCheckinAfter(SiteCheckinAfterEvent {
                site_id: "site-1".into(),
                action: "check-in".into(),
                state: "completed".into(),
                window_key: "2026-11-22".into(),
            }),
        ),
    ] {
        host.invoke_notify(
            &policy,
            Invocation::new("org.example.policy", Duration::from_secs(2)),
            Event {
                schema_version: 1,
                context: ctx("org.example.policy"),
                id: format!("ev-{kind}"),
                kind: kind.into(),
                occurred_at: 1000,
                handles: vec!["h1".into()],
                revision: 1,
                payload,
            },
        )
        .unwrap();
    }
}

fn assert_captured_downloader_commands(captured: &[CommandRequest]) {
    assert_eq!(captured.len(), 5);
    assert!(matches!(
        &captured[0].action,
        CommandAction::DownloaderAdd {
            downloader_id,
            download_ref,
            options: DownloadOptions { paused: true, .. }
        } if downloader_id == "dl-1" && download_ref == "ref-1"
    ));
    assert!(matches!(
        &captured[1].action,
        CommandAction::DownloaderControl {
            target: OwnedRevision { handle, revision: 2 },
            control: TaskControl::Pause,
        } if handle == "task-1"
    ));
    assert!(matches!(
        &captured[2].action,
        CommandAction::DownloaderDeleteTask { downloader_id, revision: 3, task_handle }
        if downloader_id == "dl-1" && task_handle == "task-1"
    ));
    assert!(matches!(
        &captured[3].action,
        CommandAction::DownloaderPrepareCleanup { targets, reason }
        if targets.len() == 1 && reason == "quota"
    ));
    assert!(matches!(
        &captured[4].action,
        CommandAction::DownloaderDeleteFiles {
            target: OwnedRevision { handle, revision: 4 },
            plan_handle,
        } if handle == "task-1" && plan_handle == "plan-1"
    ));
}

#[test]
fn real_task_and_endpoint_wasm_exercise_boost_snapshot_and_structured_downloader_commands() {
    let ports = Arc::new(BoostAndCmdPorts {
        allow_boost: true,
        captured_commands: Mutex::new(Vec::new()),
    });
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    let task = host.load_task(TASK_WASM, TASK_WORLD).unwrap();
    let endpoint = host.load_endpoint(ENDPOINT_WASM, ENDPOINT_WORLD).unwrap();

    let snap_out = host
        .invoke_task(
            &task,
            Invocation::new("builtin.boost", Duration::from_secs(2)),
            TaskInput {
                task_id: "boost-tick".into(),
                validated_input: "boost-snapshot".into(),
                job_id: "j-1".into(),
                attempt: 1,
            },
        )
        .unwrap();
    assert_eq!(
        snap_out.summary,
        "used=2000;reserved=500;retained=1000;disk=50000"
    );

    let cmd_out = host
        .invoke_task(
            &task,
            Invocation::new("builtin.boost", Duration::from_secs(2)),
            TaskInput {
                task_id: "boost-tick".into(),
                validated_input: "downloader-commands".into(),
                job_id: "j-2".into(),
                attempt: 1,
            },
        )
        .unwrap();
    assert_eq!(cmd_out.operation_ids.len(), 5);
    assert_captured_downloader_commands(&ports.captured_commands.lock().unwrap());

    // Endpoint invokes boost.snapshot
    let ep_resp = host
        .invoke_endpoint(
            &endpoint,
            Invocation::new("builtin.boost", Duration::from_secs(2)),
            EndpointRequest {
                method: "GET".into(),
                path: "/boost-snapshot".into(),
                query: vec![],
                body: vec![],
            },
        )
        .unwrap();
    assert_eq!(ep_resp.status, 200);
    assert_eq!(
        String::from_utf8_lossy(&ep_resp.body),
        "rev=7;tasks=1;res=1"
    );

    // Unauthorized read:boost rejected with PermissionDenied
    let denied_host = runtime.host(Arc::new(BoostAndCmdPorts {
        allow_boost: false,
        captured_commands: Mutex::new(Vec::new()),
    }));
    let err = denied_host
        .invoke_task(
            &task,
            Invocation::new("untrusted.plugin", Duration::from_secs(2)),
            TaskInput {
                task_id: "t".into(),
                validated_input: "boost-snapshot".into(),
                job_id: "j-3".into(),
                attempt: 1,
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        plugin_api::Failure::Guest(ApiError {
            code: ErrorCode::PermissionDenied,
            ..
        })
    ));
}

#[test]
fn boost_import_is_blocked_for_hook_world_and_lifecycle_context() {
    // Lifecycle validate calling boost.snapshot (if task_fixture invoked in lifecycle) returns WrongContext,
    // and loading a hook-plugin that imports boost is rejected by HOOK_IMPORTS allowlist.
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(Arc::new(BoostAndCmdPorts {
        allow_boost: true,
        captured_commands: Mutex::new(Vec::new()),
    }));
    // Task component cannot be loaded into hook world (wrong imports + exports).
    assert!(matches!(
        host.load(TASK_WASM, HOOK_WORLD),
        Err(plugin_api::Failure::InvalidComponent(_))
    ));
    let task = host.load_task(TASK_WASM, TASK_WORLD).unwrap();
    let err = host
        .validate_task(
            &task,
            Invocation::new("builtin.boost", Duration::from_secs(2)),
            "wrong-context-boost",
            1,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        plugin_api::Failure::Guest(ApiError {
            code: ErrorCode::WrongContext,
            ..
        })
    ));
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SeenCall {
    inv_id: String,
    plugin_id: String,
    role: plugin_api::GuestRole,
    generation: u64,
    logged_fields: Vec<(String, String)>,
}

struct ContextSpyPorts {
    begun: Mutex<Vec<(String, String, plugin_api::GuestRole, u64)>>,
    logged: Mutex<Vec<SeenCall>>,
}

impl ContextSpyPorts {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            begun: Mutex::new(Vec::new()),
            logged: Mutex::new(Vec::new()),
        })
    }
}

impl HostPorts for ContextSpyPorts {
    fn begin_invocation(
        &self,
        inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        self.begun.lock().unwrap().push((
            inv.id().to_string(),
            inv.plugin_id.clone(),
            inv.role,
            inv.generation,
        ));
        Ok(None)
    }

    fn authorized(&self, _inv: &Invocation, _cap: &str) -> bool {
        true
    }

    fn log_write(
        &self,
        inv: &Invocation,
        _level: plugin_api::LogLevel,
        message: &str,
        fields: &[(String, String)],
    ) -> Result<(), ApiError> {
        if message == "current-context" {
            self.logged.lock().unwrap().push(SeenCall {
                inv_id: inv.id().to_string(),
                plugin_id: inv.plugin_id.clone(),
                role: inv.role,
                generation: inv.generation,
                logged_fields: fields.to_vec(),
            });
        }
        Ok(())
    }
}

fn new_spy_invocation(plugin_id: &str, generation: u64, config_rev: u64) -> Invocation {
    let mut inv = Invocation::new(plugin_id, Duration::from_secs(2));
    inv.generation = generation;
    inv.config_revision = config_rev;
    inv
}

#[test]
fn runtime_current_context_matches_per_plugin_invocation_across_filter_gate_and_notify() {
    use plugin_api::GuestRole;

    let ports = ContextSpyPorts::new();
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    let inv_filter = new_spy_invocation("org.example.policy", 7, 3);
    let filter_inv_id = inv_filter.id().to_string();
    let mut stale_ctx = ctx("stale.plugin");
    stale_ctx.invocation_id = "stale-id".into();
    stale_ctx.generation = 1;
    stale_ctx.config_revision = 1;

    host.invoke_filter(
        &policy,
        inv_filter,
        FilterInput {
            schema_version: 1,
            context: stale_ctx.clone(),
            payload: FilterPayload::SearchBefore(SearchBefore {
                query: SearchQuery {
                    keyword: "verify-context".into(),
                    page: 1,
                    categories: vec![],
                },
                site_ids: vec!["site-1".into()],
            }),
        },
    )
    .unwrap();

    let inv_gate = new_spy_invocation("org.example.policy", 7, 3);
    let gate_inv_id = inv_gate.id().to_string();
    host.invoke_gate(
        &policy,
        inv_gate,
        GateInput {
            schema_version: 1,
            context: stale_ctx.clone(),
            hook_id: "search.before".into(),
            handles: vec!["verify-context".into()],
            revision: 1,
            payload: GatePayload::None,
        },
    )
    .unwrap();

    let inv_notify = new_spy_invocation("org.example.policy", 7, 3);
    let notify_inv_id = inv_notify.id().to_string();
    host.invoke_notify(
        &policy,
        inv_notify,
        Event {
            schema_version: 1,
            context: stale_ctx,
            id: "ev-ctx".into(),
            kind: "download.after".into(),
            occurred_at: 123,
            handles: vec!["verify-context".into()],
            revision: 1,
            payload: EventPayload::None,
        },
    )
    .unwrap();

    assert_eq!(
        ports.begun.lock().unwrap().clone(),
        vec![
            (
                filter_inv_id.clone(),
                "org.example.policy".into(),
                GuestRole::Filter,
                7
            ),
            (
                gate_inv_id.clone(),
                "org.example.policy".into(),
                GuestRole::Gate,
                7
            ),
            (
                notify_inv_id.clone(),
                "org.example.policy".into(),
                GuestRole::Notify,
                7
            ),
        ]
    );
    let logged = ports.logged.lock().unwrap().clone();
    assert_eq!(logged.len(), 3);
    for (entry, expected_id, expected_role) in [
        (&logged[0], &filter_inv_id, GuestRole::Filter),
        (&logged[1], &gate_inv_id, GuestRole::Gate),
        (&logged[2], &notify_inv_id, GuestRole::Notify),
    ] {
        assert_eq!(
            (
                &entry.inv_id,
                entry.plugin_id.as_str(),
                entry.role,
                entry.generation
            ),
            (expected_id, "org.example.policy", expected_role, 7)
        );
        assert!(
            entry
                .logged_fields
                .contains(&("invocation_id".into(), expected_id.clone()))
        );
        assert!(
            entry
                .logged_fields
                .contains(&("config_revision".into(), "3".into()))
        );
    }
}

#[test]
fn hook_pipeline_updates_runtime_role_between_filter_and_gate() {
    use plugin_api::GuestRole;

    let ports = ContextSpyPorts::new();
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();
    let invoker = Arc::new(PolicyInvoker::new(host, policy, || {
        new_spy_invocation("org.example.policy", 9, 5)
    }));
    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "search.before",
        10,
        "org.example.policy",
        "policy",
        invoker,
    ));
    let _ = pipeline
        .filter_typed(
            "search.before",
            &FilterInput {
                schema_version: 1,
                context: ctx("other"),
                payload: FilterPayload::SearchBefore(SearchBefore {
                    query: SearchQuery {
                        keyword: "verify-context".into(),
                        page: 1,
                        categories: vec![],
                    },
                    site_ids: vec!["site-1".into()],
                }),
            },
        )
        .unwrap();
    let _ = pipeline
        .gate_typed(&GateInput {
            schema_version: 1,
            context: ctx("other"),
            hook_id: "search.before".into(),
            handles: vec!["verify-context".into()],
            revision: 1,
            payload: GatePayload::None,
        })
        .unwrap();
    let logged = ports.logged.lock().unwrap().clone();
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[0].role, GuestRole::Filter);
    assert_eq!(logged[1].role, GuestRole::Gate);
    assert_ne!(logged[0].inv_id, logged[1].inv_id);
    assert_eq!((logged[0].generation, logged[1].generation), (9, 9));
}
