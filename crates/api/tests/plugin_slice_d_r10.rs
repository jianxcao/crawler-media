#[path = "builtin_boost/support.rs"]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginCommandTarget, PluginGrant, PluginGrantScope, PluginOperationState, PluginSubject,
    PluginVersionMetadata, StagedPluginIntent, StagedPluginOpKind,
};
use support::plugin_http_support::{HttpEnv, TOKEN, admin_id, assert_ok, pkg, req_json};
use uuid::Uuid;

const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

fn build_yaml_site_and_cmd_zip(id: &str, site_slug: &str) -> Vec<u8> {
    let yaml = format!(
        "id: {site_slug}\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n"
    );
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R10 {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R10"
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

fn plant_running_cmd(env: &HttpEnv, pid: &str) -> Uuid {
    let rec = env.state.store().get_plugin(pid).unwrap().unwrap();
    let auth = env
        .state
        .store()
        .issue_plugin_state_authority(&rec, None)
        .unwrap();
    let cmd_auth = env
        .state
        .store()
        .authorize_plugin_command(&auth, "schedule.set", &[PluginCommandTarget::Global])
        .unwrap();
    let acc = env
        .state
        .store()
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some(pid.into()),
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("r10-cmd-{pid}"),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid}/commands"),
                    body_digest: [71; 32],
                },
                kind: "schedule.set".into(),
                expected_revision: Some(rec.revision),
                expected_generation: Some(rec.generation),
                cause: PluginCommandCause::root("r10-cmd"),
            },
            b"{}",
        )
        .unwrap();
    env.state
        .store()
        .claim_plugin_command(
            acc.operation.operation_id,
            acc.operation.revision,
            Some(rec.generation),
            0,
            10,
        )
        .unwrap();
    acc.operation.operation_id
}

#[tokio::test]
async fn draining_not_drained_plugin_drops_yaml_profile_and_does_not_abort_recover_or_starve_sibling()
 {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid_a = "org.example.r10-a-blocked";
    let pid_b = "org.example.r10-b-ready";
    let grants =
        json!([{ "capability": "command:schedule.set", "scope": "Global", "sensitive": false }]);
    let inst_a = env
        .install_pkg(
            &build_yaml_site_and_cmd_zip(pid_a, "r10sitea"),
            None,
            grants.clone(),
        )
        .await;
    env.enable_plugin(pid_a, inst_a["revision"].as_i64().unwrap(), "en-a")
        .await;
    let inst_b = env
        .install_pkg(
            &build_yaml_site_and_cmd_zip(pid_b, "r10siteb"),
            None,
            grants,
        )
        .await;
    env.enable_plugin(pid_b, inst_b["revision"].as_i64().unwrap(), "en-b")
        .await;

    let prof_a = format!("plugin:{pid_a}/r10sitea");
    assert!(env.state.indexer().snapshot().profile(&prof_a).is_some());

    // Plant running runtime cmd on pid_a; calling disable transitions cmd -> Unknown and returns 409 plugin.not_drained
    let _cmd_id = plant_running_cmd(&env, pid_a);
    let rev_a = env
        .state
        .store()
        .get_plugin(pid_a)
        .unwrap()
        .unwrap()
        .revision;
    let (st_dis, _, b_dis) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_a}/disable"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_a}\"")),
                ("idempotency-key", "r10-dis-a"),
            ],
            Some(json!({})),
        ))
        .await;
    assert_eq!(
        st_dis,
        StatusCode::CONFLICT,
        "expected not_drained 409: {b_dis}"
    );
    assert!(
        env.state.indexer().snapshot().profile(&prof_a).is_none(),
        "entering Draining must immediately evict declarative YAML site profile even when drain fails"
    );

    // Put pid_b into Draining with no active workers/unresolved effects
    let rec_b = env.state.store().get_plugin(pid_b).unwrap().unwrap();
    env.state
        .store()
        .begin_plugin_lifecycle(pid_b, rec_b.revision, Uuid::new_v4(), false)
        .unwrap();

    api::plugins::recover_startup(&env.state)
        .expect("recover_startup must not fail when pid_a has NotDrained command");
    api::plugins::reconcile_pending(&env.state, 200)
        .expect("reconcile_pending must not fail when pid_a has NotDrained command");

    let after_b = env.state.store().get_plugin(pid_b).unwrap().unwrap();
    assert_eq!(
        after_b.lifecycle,
        store::plugins::PluginLifecycle::Disabled,
        "sibling Draining plugin pid_b must not be starved by earlier NotDrained pid_a"
    );
}

#[tokio::test]
async fn non_cb_task_enforces_live_plugin_state_authority() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r10-live-task";
    let grants =
        json!([{ "capability": "command:schedule.set", "scope": "Global", "sensitive": false }]);
    let inst = env
        .install_pkg(
            &build_yaml_site_and_cmd_zip(pid, "r10tasksite"),
            None,
            grants,
        )
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r10-lt-en")
        .await;

    let (_, _, tasks_list) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/tasks"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev = tasks_list["data"][0]["definition_revision"]
        .as_i64()
        .unwrap();
    let sched = tasks_list["data"][0]["schedule"].clone();
    let (st_u, _, b_u) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/tasks/tick/update"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev}\"")),
                ("idempotency-key", "r10-lt-upd"),
            ],
            Some(json!({ "enabled": true, "schedule": sched, "config": {} })),
        ))
        .await;
    let _ = assert_ok(st_u, &b_u, StatusCode::ACCEPTED);
    let (_, _, tasks_list2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/tasks"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev2 = tasks_list2["data"][0]["definition_revision"]
        .as_i64()
        .unwrap();
    let (st_r, _, b_r) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/tasks/tick/run"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev2}\"")),
                ("idempotency-key", "r10-lt-run"),
            ],
            Some(json!({ "input": {} })),
        ))
        .await;
    let run_view = assert_ok(st_r, &b_r, StatusCode::ACCEPTED);
    let job_id: domain::JobId = run_view["job_id"]
        .as_str()
        .or_else(|| run_view["result"]["job_id"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let job = env
        .state
        .jobs()
        .acquire()
        .unwrap()
        .get(job_id)
        .unwrap()
        .unwrap();

    let binding = env
        .state
        .store()
        .get_plugin_task_binding(pid, "tick")
        .unwrap()
        .unwrap();
    let rec = env.state.store().get_plugin(pid).unwrap().unwrap();
    let authority = env
        .state
        .store()
        .issue_plugin_state_authority(&rec, None)
        .unwrap();
    let invocation = api::plugins::PluginTaskInvocation {
        binding,
        plugin: rec.clone(),
        authority,
        site: None,
        job_id: job.id,
        attempt: job.attempt,
        cause: PluginCommandCause::root("r10-lt-race"),
        input_override: None,
    };
    env.state
        .store()
        .begin_plugin_lifecycle(pid, rec.revision, Uuid::new_v4(), false)
        .unwrap();
    assert!(
        env.state
            .plugins()
            .task_runner()
            .run_task(&env.state, &invocation)
            .is_err(),
        "non-credential-bound WASM task must fail live validate_plugin_state_authority when plugin is Draining"
    );
}

#[tokio::test]
async fn foreground_fail_cancel_requested_staged_op_respects_read_only_gate_before_seeding_tasks() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r10-readonly-seed";
    let hash = "f0f0f0f0".repeat(8);
    let manifest_json = serde_json::to_string(&serde_json::json!({
        "manifest_version": 1,
        "id": pid,
        "name": "R10 RO",
        "version": "1.0.0",
        "api": "1.0.0",
        "license": "MIT",
        "authors": ["Test"],
        "description": "R10",
        "config_schema": "schemas/config.schema.json",
        "state_schema": 1,
        "components": [{
            "id": "task-main",
            "entry": "components/task.wasm",
            "world": "crawler:plugin/task-plugin@1.0.0",
            "permissions": ["kv"],
            "tasks": [{
                "id": "sync",
                "schedule": { "kind": "interval", "seconds": 300 }
            }]
        }]
    }))
    .unwrap();

    let op_id = env
        .state
        .store()
        .app_write(|app| {
            let grants = vec![PluginGrant {
                capability: "kv".into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            }];
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r10-ro-seed".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: "/plugins".into(),
                    body_digest: [55; 32],
                },
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(hash.clone()),
                    version: Some("1.0.0".into()),
                    artifact_path: None,
                    state_policy: None,
                    target_config_digest: None,
                    grants: grants.clone(),
                },
                cause: PluginCommandCause::root("r10-ro"),
            })?;
            let oid = acc.operation.operation_id;
            let mut claim =
                app.claim_staged_plugin_operation(oid, acc.operation.revision, 0, 100)?;
            app.register_staged_plugin_version(
                &mut claim,
                &PluginVersionMetadata {
                    plugin_id: pid.into(),
                    package_hash: hash,
                    version: "1.0.0".into(),
                    manifest_json,
                    config_schema_json: "{}".into(),
                    state_schema: "1".into(),
                    artifact_path: "plugins/packages/missing.cmplugin".into(),
                    signing_key_id: None,
                    signature_status: "development".into(),
                    ui_metadata_ref: None,
                    credential_bound: false,
                },
                &grants,
                100,
            )?;
            let _ = app.cancel_plugin_operation_admin(oid, claim.revision(), 101)?;
            Ok(oid)
        })
        .unwrap();

    let ro_state = env.state.clone().with_instance_writable(false);
    api::plugins::fail_staged_operation(&ro_state, op_id, "install-failed").await;
    let bindings = env
        .state
        .store()
        .list_plugin_task_bindings(pid, None, None)
        .unwrap();
    assert!(
        bindings.items.is_empty(),
        "read-only foreground fail_staged_operation must not write plugin_task_bindings: {:?}",
        bindings.items
    );
    let op_after = env
        .state
        .store()
        .get_plugin_operation_admin(op_id)
        .unwrap()
        .unwrap();
    assert_eq!(op_after.operation.state, PluginOperationState::Unknown);
}
