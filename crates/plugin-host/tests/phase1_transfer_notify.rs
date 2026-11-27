use std::sync::Arc;
use std::time::Duration;

use hooks::{HookPipeline, PluginInvoker, Registration};
use plugin_api::{
    ApiError, ErrorCode, Event, EventPayload, Failure, HOOK_WORLD, HookContext,
    TransferCompletedEvent, TransferRename,
};
use plugin_host::{HostPorts, Invocation, Limits, PluginRuntime, PolicyInvoker};

const POLICY_WASM: &[u8] = include_bytes!("fixtures/policy.wasm");

struct AllowPorts;
impl HostPorts for AllowPorts {
    fn authorized(&self, _inv: &Invocation, _cap: &str) -> bool {
        true
    }
}

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

fn new_invocation() -> Invocation {
    let mut inv = Invocation::new("org.example.policy", Duration::from_secs(2));
    inv.generation = 1;
    inv.config_revision = 1;
    inv
}

#[test]
fn invoke_notify_rejects_empty_event_id_or_kind_and_preserves_failure_variants_and_cause_id() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(Arc::new(AllowPorts));
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    for (bad_id, bad_kind) in [
        ("", "transfer.completed"),
        ("  ", "transfer.completed"),
        ("ev-1", ""),
        ("ev-1", "   "),
    ] {
        let bad_ev = Event {
            schema_version: 1,
            context: ctx("org.example.policy"),
            id: bad_id.into(),
            kind: bad_kind.into(),
            occurred_at: 1_700_000_000_000,
            handles: vec![],
            revision: 1,
            payload: EventPayload::None,
        };
        let err = host
            .invoke_notify(&policy, new_invocation(), bad_ev)
            .unwrap_err();
        assert!(matches!(err, Failure::InvalidInput(_)), "got {err:?}");
    }

    let invoker = PolicyInvoker::new(host, policy, new_invocation);

    let mut ev = Event {
        schema_version: 1,
        context: ctx("org.example.policy"),
        id: "verify-transfer-completed".into(),
        kind: "transfer.completed".into(),
        occurred_at: 1_700_000_000_000,
        handles: vec!["h1".into()],
        revision: 1,
        payload: EventPayload::TransferCompleted(TransferCompletedEvent {
            media_id: "m1".into(),
            library_id: "lib-1".into(),
            relative_path: "Movies/Film.mkv".into(),
            file_count: 1,
            total_bytes: 2048,
        }),
    };
    invoker.notify_typed_result(&ev).unwrap();

    ev.id = "busy".into();
    assert!(matches!(
        invoker.notify_typed_result(&ev),
        Err(Failure::Guest(ApiError {
            code: ErrorCode::Busy,
            ..
        }))
    ));

    ev.id = "rate-limit".into();
    assert!(matches!(
        invoker.notify_typed_result(&ev),
        Err(Failure::Guest(ApiError {
            code: ErrorCode::Upstream,
            retry_after_ms: Some(250),
            ..
        }))
    ));

    ev.id = "invalid-arg".into();
    assert!(matches!(
        invoker.notify_typed_result(&ev),
        Err(Failure::Guest(ApiError {
            code: ErrorCode::InvalidArgument,
            ..
        }))
    ));

    ev.id = "trap".into();
    assert!(matches!(
        invoker.notify_typed_result(&ev),
        Err(Failure::Trap(_))
    ));
}

#[test]
fn real_policy_wasm_transfer_rename_chaining_noop_and_invalid_rollback() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(Arc::new(AllowPorts));
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    let pipeline = HookPipeline::new();
    pipeline.register(Registration::for_hook(
        "transfer.rename",
        10,
        "org.example.policy",
        "c1",
        Arc::new(PolicyInvoker::new(
            host.clone(),
            policy.clone(),
            new_invocation,
        )),
    ));
    pipeline.register(Registration::for_hook(
        "transfer.rename",
        20,
        "org.example.policy",
        "c2",
        Arc::new(PolicyInvoker::new(
            host.clone(),
            policy.clone(),
            new_invocation,
        )),
    ));

    let mut chained = TransferRename {
        media_id: "chain".into(),
        library_id: "lib-1".into(),
        source_path: "/dl/raw.mkv".into(),
        proposed_relative_path: "Movies/Film".into(),
        is_movie: true,
    };
    let trace = pipeline
        .filter_transfer_rename_with_trace("wf-chain", &ctx("host"), &mut chained)
        .unwrap();
    assert_eq!(
        chained.proposed_relative_path,
        "Movies/Film.chained.mkv.chained.mkv"
    );
    assert_eq!(trace.steps.len(), 2);

    let mut noop = TransferRename {
        media_id: "noop".into(),
        library_id: "lib-1".into(),
        source_path: "/dl/raw.mkv".into(),
        proposed_relative_path: "Movies/Original.mkv".into(),
        is_movie: true,
    };
    let noop_trace = pipeline
        .filter_transfer_rename_with_trace("wf-noop", &ctx("host"), &mut noop)
        .unwrap();
    assert_eq!(noop.proposed_relative_path, "Movies/Original.mkv");
    assert!(noop_trace.steps.iter().all(|s| s.decision == "noop"));

    let mut invalid = TransferRename {
        media_id: "invalid-path".into(),
        library_id: "lib-1".into(),
        source_path: "/dl/raw.mkv".into(),
        proposed_relative_path: "Movies/Safe.mkv".into(),
        is_movie: true,
    };
    let inv_trace = pipeline
        .filter_transfer_rename_with_trace("wf-invalid", &ctx("host"), &mut invalid)
        .unwrap();
    assert_eq!(invalid.proposed_relative_path, "Movies/Safe.mkv");
    assert!(inv_trace.steps.iter().all(|s| s.decision == "rolled_back"));

    let mut secret_err = TransferRename {
        media_id: "secret-error".into(),
        library_id: "lib-1".into(),
        source_path: "/dl/raw.mkv".into(),
        proposed_relative_path: "Movies/Safe.mkv".into(),
        is_movie: true,
    };
    let sec_trace = pipeline
        .filter_transfer_rename_with_trace("wf-secret", &ctx("host"), &mut secret_err)
        .unwrap();
    assert_eq!(secret_err.proposed_relative_path, "Movies/Safe.mkv");
    assert!(
        !sec_trace.steps[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("SECRET_")
    );
}
