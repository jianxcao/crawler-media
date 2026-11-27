use std::time::Duration;

use plugin_api::{EndpointRequest, ErrorCode, Failure, GuestRole, HOOK_WORLD, Header, PageRequest};
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{PluginCommandCause, PluginStateKind, PluginSubject, PluginTaskDefinition};

use crate::support::plugin_http_support::{HttpEnv, admin_id, pkg};

const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");
const EP_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/endpoint_fixture.wasm");

fn build_r5_multicomp_zip(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R5 Multi Comp"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Fail-closed multi-component isolation"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "task-priv"
entry = "components/task_a.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv", "blob", "command:site.check-in"]

[[components.tasks]]
id = "task-a"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300

[[components]]
id = "task-bare"
entry = "components/task_b.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "task-b"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300

[[components]]
id = "ep-priv"
entry = "components/ep_priv.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components]]
id = "ep-bare"
entry = "components/ep_bare.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = []

[[components.endpoints]]
id = "other"
method = "DELETE"
path = "/other"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components]]
id = "hook-single"
entry = "components/policy.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:search.site.after", "read:sites"]

[[components.hooks]]
id = "search.site.after"
priority = 10
"#
    );
    let cfg_schema = pkg::sample_schema(false, None);
    let any_obj = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"action":{"type":"string"}}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", any_obj.as_bytes()),
        ("schemas/ep.schema.json", any_obj.as_bytes()),
        ("components/task_a.wasm", pkg::task_wasm()),
        ("components/task_b.wasm", pkg::task_wasm()),
        ("components/ep_priv.wasm", EP_WASM),
        ("components/ep_bare.wasm", EP_WASM),
        ("components/policy.wasm", POLICY_WASM),
    ])
}

#[tokio::test]
async fn slice_d_r5_item2a_undeclared_endpoint_and_task_fail_closed() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r5-multicomp";
    let zip = build_r5_multicomp_zip(pid);
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "blob", "scope": "Global", "sensitive": false },
        { "capability": "command:site.check-in", "scope": "Global", "sensitive": false },
        { "capability": "hooks:search.site.after", "scope": "Global", "sensitive": false },
        { "capability": "read:sites", "scope": "Global", "sensitive": false }
    ]);
    let installed = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "r5-mc-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    let subject = PluginSubject::User(admin_id());

    let put_ok = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth.clone(),
            subject.clone(),
            PluginCommandCause::root("r5-ep-put"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"top-secret".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(put_ok.status, 200);

    let undeclared_ep = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth.clone(),
            subject,
            PluginCommandCause::root("r5-ep-undeclared"),
            EndpointRequest {
                method: "GET".into(),
                path: "/kv-get".into(),
                query: Vec::<Header>::new(),
                body: Vec::new(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(undeclared_ep, Failure::Guest(ref e) if e.code == ErrorCode::NotFound),
        "expected Guest(NotFound) for undeclared endpoint, got {undeclared_ep:?}"
    );

    assert_undeclared_task_fails_closed(&env, &enabled, &auth);
}

fn assert_undeclared_task_fails_closed(
    env: &HttpEnv,
    enabled: &store::plugins::PluginRecord,
    auth: &store::plugins::PluginStateAuthority,
) {
    let store = env.state.store();
    let rogue_binding = store
        .save_plugin_task_binding(
            enabled,
            "undeclared-task",
            0,
            &PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(300),
                public_config: json!({ "action": "submit-and-state" }),
            },
        )
        .unwrap();
    let job = jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginTask,
        scope: jobs::JobScope::PluginTask(domain::PluginTaskBindingId::from_uuid(
            rogue_binding.binding_id,
        )),
        status: jobs::JobStatus::Running,
        attempt: 1,
        payload: "{}".into(),
        run_after: 0,
        started_at: Some(1_700_000_000),
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: None,
        timeout_secs: None,
    };
    assert!(
        api::plugins::run_task_job(&env.state, &job).is_err(),
        "undeclared task_id must fail closed instead of inheriting task-priv permissions"
    );
    assert!(
        store
            .plugin_state_get(auth, PluginStateKind::Kv, "k/a")
            .unwrap()
            .is_none(),
        "undeclared task must never execute or write KV"
    );
}

#[tokio::test]
async fn slice_d_r5_item2b_ports_manifest_filter_fails_closed_on_ambiguous_world_and_corrupt_json()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r5-ports-iso";
    let zip = build_r5_multicomp_zip(pid);
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "blob", "scope": "Global", "sensitive": false },
        { "capability": "command:site.check-in", "scope": "Global", "sensitive": false },
        { "capability": "hooks:search.site.after", "scope": "Global", "sensitive": false },
        { "capability": "read:sites", "scope": "Global", "sensitive": false }
    ]);
    let installed = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "r5-pi-en")
        .await;
    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let ports = env.state.plugins().host_ports();

    let mut inv_ambig = Invocation::new(pid, Duration::from_secs(5));
    inv_ambig.generation = u64::try_from(enabled.generation).unwrap();
    inv_ambig.role = GuestRole::Task;
    let _g_ambig = ports.begin_invocation(&inv_ambig).unwrap();
    assert!(!ports.authorized(&inv_ambig, "kv"));
    assert_eq!(
        ports
            .kv_list(
                &inv_ambig,
                "",
                PageRequest {
                    limit: 10,
                    cursor: None,
                },
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    drop(_g_ambig);

    let mut inv_single = Invocation::new(pid, Duration::from_secs(5));
    inv_single.generation = u64::try_from(enabled.generation).unwrap();
    inv_single.role = GuestRole::Filter;
    let _g_single = ports.begin_invocation(&inv_single).unwrap();
    assert!(ports.authorized(&inv_single, "read:sites"));
    assert!(!ports.authorized(&inv_single, "kv"));
    drop(_g_single);

    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE plugin_versions SET manifest_json = '{corrupt' WHERE plugin_id = ?1",
                [pid],
            )?;
            Ok(())
        })
        .unwrap();
    let mut inv_corrupt = Invocation::new(pid, Duration::from_secs(5));
    inv_corrupt.generation = u64::try_from(enabled.generation).unwrap();
    inv_corrupt.role = GuestRole::Filter;
    let _g_corrupt = ports.begin_invocation(&inv_corrupt).unwrap();
    assert!(!ports.authorized(&inv_corrupt, "read:sites"));
    assert!(!ports.authorized(&inv_corrupt, "kv"));
}
