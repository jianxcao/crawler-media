use crate::hook_wat::HOOK_PHASE1_WAT;
use crate::task_wat::TASK_PHASE1_WAT;
use plugin_api::{
    ApiError, ENDPOINT_WORLD, ErrorCode, Event, EventPayload, Failure, FilterInput, FilterPayload,
    GateInput, GatePayload, HOOK_WORLD, HookContext, INDEXER_WORLD, MAINTENANCE_WORLD, SearchAfter,
    TASK_WORLD, TaskInput,
};
use plugin_host::{HostPorts, Invocation, Limits, NoServices, PluginRuntime};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

struct GrantPorts {
    grants: HashSet<String>,
}

impl GrantPorts {
    fn new(caps: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            grants: caps.iter().map(|c| (*c).to_string()).collect(),
        })
    }
}

impl HostPorts for GrantPorts {
    fn authorized(&self, _: &Invocation, cap: &str) -> bool {
        self.grants.contains(cap)
    }
}

fn hook_ctx() -> HookContext {
    HookContext {
        invocation_id: "inv-1".into(),
        cause_id: "cause-1".into(),
        plugin_id: "org.example.phase1".into(),
        generation: 1,
        config_revision: 1,
        subject: "system".into(),
        site_id: None,
        subscribe_id: None,
        library_id: None,
        deadline_ms: 9_999_999_999,
        cancelled: false,
    }
}

#[test]
fn hook_world_links_subscribe_catalog_and_notify_enforcing_permission_denied_then_unsupported() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ungranted_host = runtime.host(Arc::new(NoServices));
    let policy = ungranted_host
        .load(HOOK_PHASE1_WAT.as_bytes(), HOOK_WORLD)
        .expect("hook-plugin importing subscribe/catalog/notify must load cleanly");

    let flt_in = FilterInput {
        schema_version: 1,
        context: hook_ctx(),
        payload: FilterPayload::SearchAfter(SearchAfter {
            candidates: vec![],
            site_failures: vec![],
        }),
    };
    let gate_in = GateInput {
        schema_version: 1,
        context: hook_ctx(),
        hook_id: "search.before".into(),
        handles: vec![],
        revision: 1,
        payload: GatePayload::None,
    };
    let ev = Event {
        schema_version: 1,
        context: hook_ctx(),
        id: "ev-1".into(),
        kind: "download.after".into(),
        occurred_at: 1_700_000_000_000,
        handles: vec![],
        revision: 1,
        payload: EventPayload::None,
    };

    for err in [
        ungranted_host
            .invoke_filter(
                &policy,
                Invocation::new("p", Duration::from_secs(2)),
                flt_in.clone(),
            )
            .unwrap_err(),
        ungranted_host
            .invoke_gate(
                &policy,
                Invocation::new("p", Duration::from_secs(2)),
                gate_in.clone(),
            )
            .unwrap_err(),
        ungranted_host
            .invoke_notify(
                &policy,
                Invocation::new("p", Duration::from_secs(2)),
                ev.clone(),
            )
            .unwrap_err(),
    ] {
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::PermissionDenied,
                    ..
                })
            ),
            "expected PermissionDenied when ungranted, got {err:?}"
        );
    }

    let granted_host = runtime.host(GrantPorts::new(&[
        "read:subscribes",
        "read:catalog",
        "notify:send",
    ]));
    for err in [
        granted_host
            .invoke_filter(
                &policy,
                Invocation::new("p", Duration::from_secs(2)),
                flt_in,
            )
            .unwrap_err(),
        granted_host
            .invoke_gate(
                &policy,
                Invocation::new("p", Duration::from_secs(2)),
                gate_in,
            )
            .unwrap_err(),
        granted_host
            .invoke_notify(&policy, Invocation::new("p", Duration::from_secs(2)), ev)
            .unwrap_err(),
    ] {
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::Unsupported,
                    ..
                })
            ),
            "expected Unsupported from default HostPorts when granted, got {err:?}"
        );
    }
}

#[test]
fn task_world_links_reads_and_indexer_cache_enforcing_permission_denied_then_unsupported() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ungranted_host = runtime.host(Arc::new(NoServices));
    let task = ungranted_host
        .load_task(TASK_PHASE1_WAT.as_bytes(), TASK_WORLD)
        .expect("task-plugin importing subscribe/library/download/history/catalog/indexer-cache must load");

    for attempt in 1..=6 {
        let err = ungranted_host
            .invoke_task(
                &task,
                Invocation::new("p", Duration::from_secs(2)),
                TaskInput {
                    task_id: "t".into(),
                    validated_input: "{}".into(),
                    job_id: "j".into(),
                    attempt,
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::PermissionDenied,
                    ..
                })
            ),
            "attempt {attempt}: expected PermissionDenied when ungranted, got {err:?}"
        );
    }

    let granted_host = runtime.host(GrantPorts::new(&[
        "read:subscribes",
        "read:library",
        "read:downloads",
        "read:history",
        "read:catalog",
        "read:sites",
    ]));
    for attempt in 1..=6 {
        let err = granted_host
            .invoke_task(
                &task,
                Invocation::new("p", Duration::from_secs(2)),
                TaskInput {
                    task_id: "t".into(),
                    validated_input: "{}".into(),
                    job_id: "j".into(),
                    attempt,
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::Unsupported,
                    ..
                })
            ),
            "attempt {attempt}: expected Unsupported when granted, got {err:?}"
        );
    }
}

#[test]
fn cross_world_imports_remain_isolated_while_indexer_maintenance_and_endpoint_allow_reads() {
    use crate::traversal_wat::{INDEXER_TRAVERSAL_WAT, MAINT_TRAVERSAL_WAT};

    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(Arc::new(NoServices));

    // Adding `subscribe` & `catalog` imports to indexer-plugin and maintenance-plugin loads cleanly.
    let idx_with_reads = INDEXER_TRAVERSAL_WAT.replacen(
        "(core module $m",
        "(import \"crawler:plugin/subscribe@1.0.0\" (instance))\n  (import \"crawler:plugin/catalog@1.0.0\" (instance))\n  (core module $m",
        1,
    );
    host.load_indexer(idx_with_reads.as_bytes(), INDEXER_WORLD)
        .expect("indexer-plugin must allow reads imports");

    let maint_with_reads = MAINT_TRAVERSAL_WAT.replacen(
        "(core module $shim",
        "(import \"crawler:plugin/subscribe@1.0.0\" (instance))\n  (import \"crawler:plugin/history@1.0.0\" (instance))\n  (core module $shim",
        1,
    );
    host.load_maintenance(maint_with_reads.as_bytes(), MAINTENANCE_WORLD)
        .expect("maintenance-plugin must allow reads imports");

    // Cross-world forbidden imports still fail with InvalidComponent("unsupported import: ...").
    let bad_hook = HOOK_PHASE1_WAT.replacen(
        "(core module $m",
        "(import \"crawler:plugin/indexer-cache@1.0.0\" (instance))\n  (core module $m",
        1,
    );
    assert!(matches!(
        host.load(bad_hook.as_bytes(), HOOK_WORLD),
        Err(Failure::InvalidComponent(msg)) if msg.contains("unsupported import")
    ));

    let bad_task = TASK_PHASE1_WAT.replacen(
        "(core module $shim",
        "(import \"crawler:plugin/notify@1.0.0\" (instance))\n  (core module $shim",
        1,
    );
    assert!(matches!(
        host.load_task(bad_task.as_bytes(), TASK_WORLD),
        Err(Failure::InvalidComponent(msg)) if msg.contains("unsupported import")
    ));
    assert!(matches!(
        host.load_endpoint(bad_task.as_bytes(), ENDPOINT_WORLD),
        Err(Failure::InvalidComponent(msg)) if msg.contains("unsupported import")
    ));
}
