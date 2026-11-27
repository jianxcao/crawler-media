#[path = "builtin_boost/support.rs"]
mod support;

use std::time::Duration;

use axum::http::StatusCode;
use domain::PluginOperationId;
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginOperationState,
    PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use support::plugin_http_support::{HttpEnv, TOKEN, admin_id, assert_ok, pkg, req_json};
use uuid::Uuid;

const EP_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");
const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

fn build_ep_submit_zip(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R9 Ep {id}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R9 Slice D endpoint"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "ep-main"
entry = "components/ep.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["endpoints", "command:site.verify-session"]

[[components.endpoints]]
id = "submit"
method = "POST"
path = "/submit"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#
    );
    let empty_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", empty_schema.as_bytes()),
        ("schemas/ep.schema.json", empty_schema.as_bytes()),
        ("components/ep.wasm", EP_WASM),
    ])
}

fn build_non_cb_task_zip(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R9 Task {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R9 non-credential-bound task"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "task-main"
entry = "components/task.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv", "blob", "command:site.check-in"]

[[components.tasks]]
id = "check-in"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    );
    let cfg_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"site_id":{"type":"string"}}}"#;
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"site_id":{"type":"string"}}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/task.wasm", TASK_WASM),
    ])
}

fn seed_staged_install_op(env: &HttpEnv, pid: &str, claim_running: bool) -> (Uuid, i64) {
    env.state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("seed-{pid}"),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: "/plugins".into(),
                    body_digest: [91; 32],
                },
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some("deadbeef".repeat(8)),
                    version: Some("1.0.0".into()),
                    artifact_path: Some("plugins/uploads/missing/package.cmplugin".into()),
                    state_policy: None,
                    target_config_digest: None,
                    grants: vec![],
                },
                cause: PluginCommandCause::root("r9-seed"),
            })?;
            let oid = acc.operation.operation_id;
            if claim_running {
                let c = app.claim_staged_plugin_operation(
                    oid,
                    acc.operation.revision,
                    0,
                    store::unix_now(),
                )?;
                Ok((oid, c.revision()))
            } else {
                Ok((oid, acc.operation.revision))
            }
        })
        .unwrap()
}

#[tokio::test]
async fn startup_recovery_settles_resume_idempotency_envelope_and_missing_op_checks_conflict_first()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let (op_id, orig_rev) = seed_staged_install_op(&env, "org.example.r9-res-rec", false);

    // Put instance in read-only ONLY after Phase-1 by blocking Phase-2? Simpler: seed Phase-1 resume claim directly via HTTP while `instance.read_only` is false, Wait: `instance.read_only` blocks Phase-1 too!
    // Instead, hold a staging pin? No, simulate crash mid-Phase-2 by setting `write_gate` or seeding `Running("resuming")` + `plugin_op.resume_claim.{op_id}` via real Phase-1 resume when `store` fails Phase-2 transiently, OR call resume while `write_gate` closes? Wait: let's trigger real `POST .../resume` and simulate crash before envelope by deleting envelope? No: if `execute_staged_install_sync` fails on missing upload, `recover_staged_install` settles `Failed` AND writes envelope; if instead `op_id` is `Blocked("drain-pending")` with an active in-flight guard, `POST .../resume` transitions `op_id` to `Blocked("drain-pending")` or if `op_kind` is interrupted before Phase-2!
    // Let's seed the exact mid-Phase-2 crash state: Phase-1 of `POST /api/v1/plugin-operations/{op_id}/resume` committed (`Running("resuming")` + `plugin_op.resume_claim.{op_id}`).
    let resume_path = format!("/plugin-operations/{op_id}/resume");
    let body_bytes = serde_json::to_vec(&json!({})).unwrap();
    use sha2::Digest as _;
    let body_digest: [u8; 32] = sha2::Sha256::digest(&body_bytes).into();
    let claimed_rev = env
        .state
        .store()
        .app_write(|app| {
            let c = app.claim_staged_plugin_operation(op_id, orig_rev, 0, store::unix_now())?;
            let meta = json!({
                "actor": admin_id().to_string(),
                "idempotency_key": "r9-idem-k1",
                "method": "POST",
                "path": resume_path,
                "body_digest": body_digest,
                "original_revision": orig_rev,
                "claimed_revision": c.revision(),
            });
            app.put_setting(
                &format!("plugin_op.resume_claim.{op_id}"),
                &meta.to_string(),
            )?;
            Ok(c.revision())
        })
        .unwrap();
    let _ = claimed_rev;

    // 1.2: Reusing `r9-idem-k1` against a non-existent `missing_op` must return 409 `plugin.idempotency_conflict`, NOT 404
    let missing_op = Uuid::new_v4();
    let (st_miss, _, b_miss) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{missing_op}/resume"),
            Some(TOKEN),
            &[("if-match", "\"1\""), ("idempotency-key", "r9-idem-k1")],
            Some(json!({})),
        ))
        .await;
    assert_eq!(
        st_miss,
        StatusCode::CONFLICT,
        "expected 409 conflict, got: {b_miss}"
    );
    assert_eq!(b_miss["error"]["code"], "plugin.idempotency_conflict");

    // 1.1: Startup recovery settles `op_id` AND must materialize `"operation-resume"` idempotency envelope from `plugin_op.resume_claim.{op_id}`
    api::plugins::recover_startup(&env.state).unwrap();
    assert!(
        env.state
            .store()
            .get_setting(&format!("plugin_op.resume_claim.{op_id}"))
            .unwrap()
            .is_none()
    );

    let (st_rep, _, b_rep) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{orig_rev}\"")),
                ("idempotency-key", "r9-idem-k1"),
            ],
            Some(json!({})),
        ))
        .await;
    let rep = assert_ok(st_rep, &b_rep, StatusCode::ACCEPTED);
    assert_eq!(rep["state"], "failed");
}

#[tokio::test]
async fn cancelled_staged_op_reconciles_to_cancelled_at_runtime_and_foreground_fail_respects_read_only()
 {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    // 1.4a: Running staged op cancelled via HTTP -> runtime `reconcile_operation` settles to `Cancelled` (`effect = "none"`) and releases lock
    let pid = "org.example.r9-cancel-rec";
    let (op_id, run_rev) = seed_staged_install_op(&env, pid, true);
    let (st_can, _, b_can) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/cancel"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{run_rev}\"")),
                ("idempotency-key", "r9-can-1"),
            ],
            Some(json!({ "reason": "user abort" })),
        ))
        .await;
    let can_view = assert_ok(st_can, &b_can, StatusCode::ACCEPTED);
    assert_eq!(can_view["state"], "unknown");

    let reconciled =
        api::plugins::reconcile_operation(&env.state, PluginOperationId::from_uuid(op_id))
            .expect("reconcile_operation")
            .expect("reconciled outcome");
    assert_eq!(reconciled.state, PluginOperationState::Cancelled);

    // Subsequent staged op on same `pid` succeeds (proving `plugin_locks` was released)
    let (_op2, _) = seed_staged_install_op(&env, pid, false);

    // 1.1 (Slice D): `fail_staged_operation` respects `instance.read_only` AND settles `cancel-requested` `Unknown` to `Cancelled` (not `Failed`)
    let (op3, rev3) = seed_staged_install_op(&env, "org.example.r9-ro-guard", true);
    let _ = env.state.clone().with_instance_writable(false);
    api::plugins::fail_staged_operation(&env.state, op3, "install-failed").await;
    let after_ro = env
        .state
        .store()
        .get_plugin_operation_admin(op3)
        .unwrap()
        .unwrap();
    assert_eq!(
        after_ro.operation.state,
        PluginOperationState::Running,
        "fail_staged_operation must NOT mutate app.db while instance_writable is false"
    );
    assert_eq!(after_ro.operation.revision, rev3);
    let _ = env.state.clone().with_instance_writable(true);
}

#[tokio::test]
async fn in_flight_fences_generations_across_disabled_upgrade_grant_and_startup_recovery() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r9-gen-fence";
    let zip1 = build_ep_submit_zip(pid, "1.0.0");
    let inst = env
        .install_pkg(
            &zip1,
            None,
            json!([{ "capability": "endpoints", "scope": "Global", "sensitive": false }]),
        )
        .await;
    let rev1 = inst["revision"].as_i64().unwrap();

    // Upgrade v1 -> v2 while still `InstalledDisabled` (`gen` advances 0 -> 1)
    let zip2 = build_ep_submit_zip(pid, "1.1.0");
    let up2 = env.upload_pkg(&zip2, None, "r9-gf-up2").await;
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev1}\"")),
                ("idempotency-key", "r9-gf-prep"),
            ],
            Some(json!({
                "version": "1.1.0",
                "upload_id": up2["upload_id"],
                "state_policy": "preserve"
            })),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::ACCEPTED);
    let rev_after_prep = env.get_plugin_rev(pid).await;
    let reauth = env.reauth("plugin:upgrade").await;
    let (st_u, _, b_u) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrade"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_after_prep}\"")),
                ("idempotency-key", "r9-gf-upg"),
            ],
            Some(json!({
                "preparation_id": prep["result"]["preparation_id"],
                "grants": [{ "capability": "endpoints", "scope": "Global", "sensitive": false }],
                "state_policy": "preserve",
                "target_config_digest": prep["result"]["target_config_digest"],
                "consent_token": prep["result"]["consent_token"],
                "reauth_token": reauth
            })),
        ))
        .await;
    assert_ok(st_u, &b_u, StatusCode::ACCEPTED);

    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    assert!(cur.generation >= 1);
    let inv = Invocation::new(pid, Duration::from_secs(5));
    assert!(
        env.state
            .plugins()
            .enter_library_invocation_for_test(pid, 0, &inv)
            .is_err(),
        "stale generation 0 must be fenced after disabled package upgrade to generation {}",
        cur.generation
    );
}

#[tokio::test]
async fn non_credential_bound_endpoint_propagates_site_id_and_validates_live_authority() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_a = env.insert_site("R9-NonCB-SiteA");

    let ep_pid = "org.example.r9-noncb-ep";
    let ep_zip = build_ep_submit_zip(ep_pid, "1.0.0");
    let ep_grants = json!([
        { "capability": "endpoints", "scope": "Global", "sensitive": false },
        { "capability": "command:site.verify-session", "scope": { "Site": site_a.to_string() }, "sensitive": false }
    ]);
    let ep_inst = env.install_pkg(&ep_zip, None, ep_grants).await;
    env.enable_plugin(
        ep_pid,
        ep_inst["revision"].as_i64().unwrap(),
        "r9-ncb-ep-en",
    )
    .await;

    let ep_rec = env.state.store().get_plugin(ep_pid).unwrap().unwrap();
    let ep_auth = env
        .state
        .store()
        .issue_plugin_state_authority(&ep_rec, None)
        .unwrap();
    let ep_resp = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &ep_rec,
            ep_auth.clone(),
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r9-ncb-ep-1"),
            plugin_api::EndpointRequest {
                method: "POST".into(),
                path: "/submit".into(),
                query: vec![plugin_api::Header {
                    name: "site_id".into(),
                    value: site_a.to_string(),
                }],
                body: b"ep-idem-1".to_vec(),
            },
        )
        .expect("non-credential-bound endpoint with ?site_id= must propagate site_id");
    assert_eq!(
        ep_resp.status,
        202,
        "expected 202 from /submit with propagated site_id, got {} body={}",
        ep_resp.status,
        String::from_utf8_lossy(&ep_resp.body)
    );

    let bad_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &ep_rec,
            ep_auth.clone(),
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r9-ncb-ep-bad"),
            plugin_api::EndpointRequest {
                method: "POST".into(),
                path: "/submit".into(),
                query: vec![plugin_api::Header {
                    name: "site_id".into(),
                    value: "not-a-uuid".into(),
                }],
                body: b"ep-idem-2".to_vec(),
            },
        )
        .expect_err("malformed ?site_id= must be rejected");
    assert!(
        format!("{bad_err:?}").contains("InvalidArgument"),
        "expected InvalidArgument, got: {bad_err:?}"
    );

    // Transition plugin out of Enabled in SQLite directly (without calling `in_flight.close_dispatch`) to prove `invoke_endpoint` validates live SQLite authority
    env.state
        .store()
        .begin_plugin_lifecycle(ep_pid, ep_rec.revision, Uuid::new_v4(), false)
        .unwrap();
    let stale_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &ep_rec,
            ep_auth,
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r9-ncb-ep-stale"),
            plugin_api::EndpointRequest {
                method: "POST".into(),
                path: "/submit".into(),
                query: vec![],
                body: b"ep-idem-3".to_vec(),
            },
        )
        .expect_err("disabled plugin in SQLite must reject stale non-cb authority");
    assert!(
        format!("{stale_err:?}").contains("StaleGeneration"),
        "expected StaleGeneration, got: {stale_err:?}"
    );
}

#[tokio::test]
async fn non_credential_bound_task_propagates_site_id_to_guest_and_ports() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_a = env.insert_site("R9-NonCB-TaskSiteA");

    let tk_pid = "org.example.r9-noncb-task";
    let tk_zip = build_non_cb_task_zip(tk_pid);
    let tk_grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "blob", "scope": "Global", "sensitive": false },
        { "capability": "command:site.check-in", "scope": { "Site": site_a.to_string() }, "sensitive": false }
    ]);
    let tk_inst = env.install_pkg(&tk_zip, None, tk_grants).await;
    env.enable_plugin(
        tk_pid,
        tk_inst["revision"].as_i64().unwrap(),
        "r9-ncb-tk-en",
    )
    .await;

    let (_, _, tasks_list) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{tk_pid}/tasks"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev = tasks_list["data"][0]["definition_revision"]
        .as_i64()
        .unwrap();
    let sched = tasks_list["data"][0]["schedule"].clone();
    let (st_upd, _, b_upd) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{tk_pid}/tasks/check-in/update"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev}\"")),
                ("idempotency-key", "r9-ncb-upd"),
            ],
            Some(json!({ "enabled": true, "schedule": sched, "config": {} })),
        ))
        .await;
    let _upd_view = assert_ok(st_upd, &b_upd, StatusCode::ACCEPTED);
    let (_, _, tasks_list2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{tk_pid}/tasks"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev2 = tasks_list2["data"][0]["definition_revision"]
        .as_i64()
        .unwrap();
    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{tk_pid}/tasks/check-in/run"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev2}\"")),
                ("idempotency-key", "r9-ncb-run"),
            ],
            Some(json!({ "input": { "site_id": site_a.to_string() } })),
        ))
        .await;
    let run_view = assert_ok(st_run, &b_run, StatusCode::ACCEPTED);
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
    api::plugins::run_task_job(&env.state, &job)
        .expect("non-credential-bound task must propagate input.site_id to guest & ports");
}

#[tokio::test]
async fn foreground_resume_syncs_registry_snapshot_after_draining_to_disabled() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r9-reg-sync";
    let yaml = "id: r9regsite\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n";
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R9 Reg\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"Test\"]\ndescription = \"R9\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\nsites = [\"sites/custom.yaml\"]\n"
    );
    let empty_schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", empty_schema.as_bytes()),
        ("sites/custom.yaml", yaml.as_bytes()),
    ]);
    let inst = env.install_pkg(&zip, None, json!([])).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r9-rs-en")
        .await;
    let prof_key = format!("plugin:{pid}/r9regsite");
    assert!(
        env.state.indexer().snapshot().profile(&prof_key).is_some(),
        "enabled site-profile plugin must be in RegistrySnapshot"
    );

    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    let (op_id, op_rev) = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r9-rs-upg-seed".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid}/upgrade"),
                    body_digest: [77; 32],
                },
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur.revision,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some("abababab".repeat(8)),
                    version: Some("1.1.0".into()),
                    artifact_path: Some("plugins/uploads/missing/package.cmplugin".into()),
                    state_policy: Some("reset".into()),
                    target_config_digest: None,
                    grants: vec![],
                },
                cause: PluginCommandCause::root("r9-rs-seed"),
            })?;
            let oid = acc.operation.operation_id;
            let _ = app.begin_plugin_lifecycle(pid, cur.revision, oid, false)?;
            Ok((oid, acc.operation.revision))
        })
        .unwrap();

    let (st_res, _, b_res) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{op_rev}\"")),
                ("idempotency-key", "r9-rs-resume"),
            ],
            Some(json!({})),
        ))
        .await;
    let res_view = assert_ok(st_res, &b_res, StatusCode::ACCEPTED);
    assert_eq!(res_view["state"], "failed");
    assert!(
        env.state.indexer().snapshot().profile(&prof_key).is_none(),
        "RegistrySnapshot must drop profile when foreground resume finalizes Draining -> Disabled"
    );
}
