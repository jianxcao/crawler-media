#[path = "builtin_boost/support.rs"]
mod support;

use std::time::Duration;

use axum::http::StatusCode;
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{
    AcceptPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginGrant,
    PluginGrantScope, PluginLifecycle, PluginOperationState, PluginSubject,
};
use support::plugin_http_support::{HttpEnv, TOKEN, admin_id, pkg, req_json};
use uuid::Uuid;

const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

fn build_r11_yaml_site_zip(id: &str, site_slug: &str) -> Vec<u8> {
    let yaml = format!(
        "id: {site_slug}\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n"
    );
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R11 {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R11"
config_schema = "config.schema.json"
state_schema = 1
sites = ["sites/custom.yaml"]

[[components]]
id = "task-main"
entry = "components/task.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["command:schedule.set"]

[[components.tasks]]
id = "tick"
input_schema = "task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    );
    let empty_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", empty_schema.as_bytes()),
        ("task.schema.json", empty_schema.as_bytes()),
        ("sites/custom.yaml", yaml.as_bytes()),
        ("components/task.wasm", TASK_WASM),
    ])
}

#[tokio::test]
async fn prepare_install_rejects_mismatched_plugin_id_or_version_hints() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r11-hints";
    let zip = build_r11_yaml_site_zip(pid, "r11hints");
    let up = env.upload_pkg(&zip, None, "r11-up-hints").await;
    let upload_id = up["upload_id"].as_str().unwrap();

    let (st_id, _, body_id) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[
                ("if-match", "\"0\""),
                ("idempotency-key", "r11-mismatch-id"),
            ],
            Some(json!({
                "upload_id": upload_id,
                "plugin_id": "org.example.other"
            })),
        ))
        .await;
    assert_eq!(st_id, StatusCode::BAD_REQUEST, "{body_id:?}");
    assert_eq!(body_id["error"]["code"], "plugin.invalid_package");

    let (st_ver, _, body_ver) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[
                ("if-match", "\"0\""),
                ("idempotency-key", "r11-mismatch-ver"),
            ],
            Some(json!({
                "upload_id": upload_id,
                "plugin_id": pid,
                "version": "2.9.9"
            })),
        ))
        .await;
    assert_eq!(st_ver, StatusCode::BAD_REQUEST, "{body_ver:?}");
    assert_eq!(body_ver["error"]["code"], "plugin.invalid_package");
}

#[tokio::test]
async fn reconcile_recovers_orphaned_running_draining_op_and_evicts_draining_yaml() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r11-reconcile";
    let grants =
        json!([{ "capability": "command:schedule.set", "scope": "Global", "sensitive": false }]);
    let inst = env
        .install_pkg(&build_r11_yaml_site_zip(pid, "r11site"), None, grants)
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r11-en")
        .await;
    let prof_id = format!("plugin:{pid}/r11site");
    assert!(
        env.state.indexer().snapshot().profile(&prof_id).is_some(),
        "enabled YAML site profile should be published"
    );

    // Simulate an orphaned foreground disable operation whose worker exited after claiming Running
    let rec = env.state.store().get_plugin(pid).unwrap().unwrap();
    let acc_disable = env
        .state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.into()),
            subject: PluginSubject::User(admin_id()),
            idempotency_key: "r11-orphan-disable".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{pid}/disable"),
                body_digest: [0; 32],
            },
            kind: "disable".into(),
            expected_revision: Some(rec.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("r11-orphan-disable"),
        })
        .unwrap();
    let disable_op_id = acc_disable.operation.operation_id;
    let draining = env
        .state
        .store()
        .begin_plugin_lifecycle(pid, rec.revision, disable_op_id, false)
        .unwrap();
    env.state
        .store()
        .claim_plugin_command(disable_op_id, acc_disable.operation.revision, None, 0, 100)
        .unwrap();

    // Hold an in-flight guard so Draining cannot finalize yet; reconcile must still evict YAML profile
    let hold_inv = Invocation::new(pid.to_string(), Duration::from_secs(30));
    let hold_guard = env
        .state
        .plugins()
        .enter_library_invocation_for_test(pid, draining.generation, &hold_inv)
        .unwrap();
    api::plugins::reconcile_pending(&env.state, 101).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin(pid)
            .unwrap()
            .unwrap()
            .lifecycle,
        PluginLifecycle::Draining
    );
    assert!(
        env.state.indexer().snapshot().profile(&prof_id).is_none(),
        "Draining YAML site profile must be evicted even while workers are still finishing"
    );

    drop(hold_guard);
    api::plugins::reconcile_pending(&env.state, 102).unwrap();
    let disabled = env.state.store().get_plugin(pid).unwrap().unwrap();
    assert_eq!(disabled.lifecycle, PluginLifecycle::Disabled);
    let disable_admin = env
        .state
        .store()
        .get_plugin_operation_admin(disable_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        disable_admin.operation.state,
        PluginOperationState::Succeeded
    );
}

#[tokio::test]
async fn reconcile_aborts_orphaned_running_enabling_operation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r11-enabling";
    let grants =
        json!([{ "capability": "command:schedule.set", "scope": "Global", "sensitive": false }]);
    let _inst = env
        .install_pkg(&build_r11_yaml_site_zip(pid, "r11enabling"), None, grants)
        .await;
    let installed = env.state.store().get_plugin(pid).unwrap().unwrap();

    let acc_enable = env
        .state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid.into()),
            subject: PluginSubject::User(admin_id()),
            idempotency_key: "r11-orphan-enable".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{pid}/enable"),
                body_digest: [1; 32],
            },
            kind: "enable".into(),
            expected_revision: Some(installed.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("r11-orphan-enable"),
        })
        .unwrap();
    let enable_op_id = acc_enable.operation.operation_id;
    env.state
        .store()
        .begin_plugin_enable_checked(pid, installed.revision, enable_op_id)
        .unwrap();
    env.state
        .store()
        .claim_plugin_command(enable_op_id, acc_enable.operation.revision, None, 0, 103)
        .unwrap();

    api::plugins::reconcile_pending(&env.state, 104).unwrap();
    let aborted = env.state.store().get_plugin(pid).unwrap().unwrap();
    assert!(matches!(
        aborted.lifecycle,
        PluginLifecycle::Disabled | PluginLifecycle::InstalledDisabled
    ));
    let enable_admin = env
        .state
        .store()
        .get_plugin_operation_admin(enable_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(enable_admin.operation.state, PluginOperationState::Failed);
}

#[tokio::test]
async fn builtin_task_runner_enforces_live_authority_and_in_flight_dispatch_fence() {
    let env = HttpEnv::new().await;
    let pid = "builtin.checkin";
    let mut enabled = env.state.store().get_plugin(pid).unwrap().unwrap();
    if enabled.lifecycle != PluginLifecycle::Enabled {
        env.state
            .store()
            .grant_plugin_permissions(
                pid,
                &enabled.package_hash,
                enabled.revision,
                &[PluginGrant {
                    capability: "read:sites".into(),
                    scope: PluginGrantScope::Global,
                    sensitive: false,
                }],
            )
            .unwrap();
        let updated = env.state.store().get_plugin(pid).unwrap().unwrap();
        env.enable_plugin(pid, updated.revision, "r11-checkin-en")
            .await;
        enabled = env.state.store().get_plugin(pid).unwrap().unwrap();
    }

    let authority = env
        .state
        .store()
        .issue_plugin_state_authority(&enabled, None)
        .unwrap();
    let binding = env
        .state
        .store()
        .list_plugin_task_bindings(pid, None, Some(10))
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let invocation = api::plugins::PluginTaskInvocation {
        job_id: domain::JobId::new(),
        attempt: 1,
        plugin: enabled.clone(),
        authority,
        binding,
        cause: PluginCommandCause::root("r11-builtin-task"),
        site: None,
        input_override: None,
    };

    // Transition builtin.checkin to Draining after authority was issued
    env.state
        .store()
        .begin_plugin_lifecycle(pid, enabled.revision, Uuid::new_v4(), false)
        .unwrap();

    let err = env
        .state
        .plugins()
        .task_runner()
        .run_task(&env.state, &invocation)
        .expect_err("stale authority on draining builtin plugin must be rejected");
    assert!(
        err.contains("stale") || err.contains("enabled") || err.contains("authority"),
        "unexpected error: {err}"
    );
}
