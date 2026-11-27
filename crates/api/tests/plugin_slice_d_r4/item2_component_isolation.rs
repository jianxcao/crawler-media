use std::time::Duration;

use axum::http::StatusCode;
use plugin_api::{EndpointRequest, ErrorCode, Failure, GuestRole, Header, PageRequest};
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{PluginCommandCause, PluginSubject};

use crate::post_write;
use crate::support::plugin_http_support::{HttpEnv, admin_id, assert_ok, pkg};

fn build_multi_comp_zip(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "Multi Comp Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Multi component isolation test"
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
id = "ep-writer"
entry = "components/ep_writer.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components]]
id = "ep-reader-no-kv"
entry = "components/ep_reader.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = []

[[components.endpoints]]
id = "kv-get"
method = "GET"
path = "/kv-get"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components]]
id = "ep-third"
entry = "components/ep_third.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = []

[[components.endpoints]]
id = "other"
method = "DELETE"
path = "/other"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#
    );
    let cfg_schema = pkg::sample_schema(false, None);
    let any_obj = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"action":{"type":"string"}}}"#;
    let ep_wasm = include_bytes!("../../../plugin-host/tests/fixtures/endpoint_fixture.wasm");
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", any_obj.as_bytes()),
        ("schemas/ep.schema.json", any_obj.as_bytes()),
        ("components/task_a.wasm", pkg::task_wasm()),
        ("components/task_b.wasm", pkg::task_wasm()),
        ("components/ep_writer.wasm", ep_wasm),
        ("components/ep_reader.wasm", ep_wasm),
        ("components/ep_third.wasm", ep_wasm),
    ])
}

#[tokio::test]
async fn slice_d_r4_item2a_multi_component_endpoint_and_task_cache_and_permission_isolation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r4-multicomp";
    let zip = build_multi_comp_zip(pid);
    // Note: NO "endpoints" grant in effective_grants (preserves T1-F01-03)!
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "blob", "scope": "Global", "sensitive": false },
        { "capability": "command:site.check-in", "scope": "Global", "sensitive": false }
    ]);
    let installed = env.install_pkg(&zip, None, grants).await;
    let pkg_hash = installed["package_hash"].as_str().unwrap().to_string();
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "r4-mc-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    let subject = PluginSubject::User(admin_id());

    // 1. Endpoint writer component (has "kv") succeeds without "endpoints" runtime grant.
    let put_resp = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth.clone(),
            subject.clone(),
            PluginCommandCause::root("r4-ep-put"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"isolated-secret".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(put_resp.status, 200);

    // 2. Endpoint reader component (permissions = []) MUST NOT inherit plugin-level "kv" grant!
    let get_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth.clone(),
            subject.clone(),
            PluginCommandCause::root("r4-ep-get"),
            EndpointRequest {
                method: "GET".into(),
                path: "/kv-get".into(),
                query: Vec::<Header>::new(),
                body: Vec::new(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(get_err, Failure::Guest(ref e) if e.code == ErrorCode::PermissionDenied),
        "expected PermissionDenied for unprivileged endpoint component, got {get_err:?}"
    );

    // 3. Corrupt ep_third.wasm on disk AFTER ep_writer.wasm was cached: proves cache keys by entry!
    let pkg_dir = store.data_dir().join("plugins/packages").join(&pkg_hash);
    std::fs::write(pkg_dir.join("components/ep_third.wasm"), b"not-valid-wasm").unwrap();
    let third_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth,
            subject,
            PluginCommandCause::root("r4-ep-third"),
            EndpointRequest {
                method: "DELETE".into(),
                path: "/other".into(),
                query: Vec::<Header>::new(),
                body: Vec::new(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(third_err, Failure::InvalidComponent(_)),
        "expected InvalidComponent loading distinct ep_third.wasm entry, got {third_err:?}"
    );

    assert_multi_task_isolation_and_cache(&env, pid, &pkg_dir).await;
}

async fn assert_multi_task_isolation_and_cache(
    env: &HttpEnv,
    pid: &str,
    pkg_dir: &std::path::Path,
) {
    let store = env.state.store();
    for tid in ["task-a", "task-b"] {
        let b = store.get_plugin_task_binding(pid, tid).unwrap().unwrap();
        let (st, body) = post_write(
            env,
            format!("/api/v1/plugins/{pid}/tasks/{tid}/update"),
            b.definition_revision,
            format!("r4-en-{tid}"),
            json!({
                "enabled": true,
                "schedule": { "interval_secs": 300 },
                "config": { "action": "submit-and-state" }
            }),
        )
        .await;
        assert_ok(st, &body, StatusCode::ACCEPTED);
    }

    let b_b = store
        .get_plugin_task_binding(pid, "task-b")
        .unwrap()
        .unwrap();
    let job_b = jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginTask,
        scope: jobs::JobScope::PluginTask(domain::PluginTaskBindingId::from_uuid(b_b.binding_id)),
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
    let res_b = api::plugins::run_task_job(&env.state, &job_b);
    assert!(
        res_b.is_err(),
        "task-b without kv/blob permissions must fail state write"
    );

    std::fs::write(pkg_dir.join("components/task_a.wasm"), b"corrupt-wasm").unwrap();
    let b_a = store
        .get_plugin_task_binding(pid, "task-a")
        .unwrap()
        .unwrap();
    let job_a = jobs::Job {
        scope: jobs::JobScope::PluginTask(domain::PluginTaskBindingId::from_uuid(b_a.binding_id)),
        ..job_b
    };
    let res_a = api::plugins::run_task_job(&env.state, &job_a);
    assert!(
        res_a.is_err(),
        "task-a must load its own entry task_a.wasm rather than reusing task_b cache"
    );
}

fn build_idx_hook_zip(pid: &str) -> Vec<u8> {
    let idx_wasm = include_bytes!("../../../plugin-host/tests/fixtures/indexer_fixture.wasm");
    let pol_wasm = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");
    let site_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let cfg_schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "Indexer Isolation"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Indexer and Hook isolation"
config_schema = "schemas/config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "idx-priv"
entry = "components/idx_a.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide", "kv", "blob"]

[[components.indexers]]
id = "fixture"
name = "Demo Priv"
kind = "torznab"
capabilities = ["search", "rss", "detail", "resolve-download", "user-stats", "categories"]
site_schema = "schemas/site.schema.json"

[[components]]
id = "hook-bare"
entry = "components/hook.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:search.site.after"]

[[components.hooks]]
id = "search.site.after"
priority = 100
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/site.schema.json", site_schema.as_bytes()),
        ("components/idx_a.wasm", idx_wasm),
        ("components/hook.wasm", pol_wasm),
    ])
}

#[tokio::test]
async fn slice_d_r4_item2b_ports_provider_component_isolation_and_blob_role_restriction() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R4-IdxSite");
    let store = env.state.store();

    let pid = "org.example.r4-idx-iso";
    let zip = build_idx_hook_zip(pid);
    let grants = json!([
        { "capability": "indexer:provide", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "kv", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "blob", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "hooks:search.site.after", "scope": { "Site": site_id }, "sensitive": false }
    ]);
    let installed = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "r4-idx-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let obs = store.observe_site(site_id).unwrap().unwrap();
    let epoch = obs.credential_epoch().storage_key().to_string();
    let ports = env.state.plugins().host_ports();

    let mut inv_hook = Invocation::new(pid, Duration::from_secs(5));
    inv_hook.generation = u64::try_from(enabled.generation).unwrap();
    inv_hook.role = GuestRole::Filter;
    inv_hook.site_id = Some(site_id.to_string());
    inv_hook.credential_epoch = Some(epoch.clone());
    let _gh = ports.begin_invocation(&inv_hook).unwrap();
    assert!(!ports.authorized(&inv_hook, "kv"));
    assert_eq!(
        ports.kv_get(&inv_hook, "k1").err().unwrap().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        ports
            .kv_list(
                &inv_hook,
                "",
                PageRequest {
                    limit: 10,
                    cursor: None
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    drop(_gh);

    let mut inv_idx = Invocation::new(pid, Duration::from_secs(5));
    inv_idx.generation = u64::try_from(enabled.generation).unwrap();
    inv_idx.role = GuestRole::Indexer;
    inv_idx.provider_id = Some("fixture".into());
    inv_idx.site_id = Some(site_id.to_string());
    inv_idx.credential_epoch = Some(epoch.clone());
    let _gi = ports.begin_invocation(&inv_idx).unwrap();
    assert!(ports.kv_put(&inv_idx, "k1", b"v1", None).is_ok());
    assert_eq!(
        ports
            .blob_put(&inv_idx, "b1", b"v1", None)
            .unwrap_err()
            .code,
        ErrorCode::WrongContext
    );
    assert_eq!(
        ports.blob_delete(&inv_idx, "b1", None).unwrap_err().code,
        ErrorCode::WrongContext
    );
    drop(_gi);

    let mut inv_bad = Invocation::new(pid, Duration::from_secs(5));
    inv_bad.generation = u64::try_from(enabled.generation).unwrap();
    inv_bad.role = GuestRole::Indexer;
    inv_bad.provider_id = Some("other-provider".into());
    inv_bad.site_id = Some(site_id.to_string());
    inv_bad.credential_epoch = Some(epoch);
    assert_eq!(
        ports.begin_invocation(&inv_bad).err().unwrap().code,
        ErrorCode::PermissionDenied
    );
}
