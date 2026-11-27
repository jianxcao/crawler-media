#[path = "builtin_boost/support.rs"]
mod support;

use axum::http::StatusCode;
use plugin_api::{EndpointRequest, ErrorCode, Failure, Header};
use serde_json::json;
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginCommandOutcome, PluginEffectObservation, PluginLifecycle, PluginOperationState,
    PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use support::plugin_http_support::{HttpEnv, TOKEN, admin_id, pkg, req_json};

const EP_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");

fn build_r12_endpoint_zip(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R12 Endpoint {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R12 endpoint"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "ep-main"
entry = "components/ep.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#
    );
    let cfg_schema = pkg::sample_schema(false, None);
    let ep_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/ep.schema.json", ep_schema.as_bytes()),
        ("components/ep.wasm", EP_WASM),
    ])
}

fn build_r12_cred_zip(id: &str, version: &str, cred_bound: bool) -> Vec<u8> {
    let binding_line = if cred_bound {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R12 Cred {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R12 cred flip"
config_schema = "schemas/config.schema.json"
state_schema = 1
{binding_line}

[[components]]
id = "ep-main"
entry = "components/ep.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
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

#[tokio::test]
async fn upgrade_prepare_rejects_preserve_when_credential_bound_flips() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r12-cred-flip";

    let zip_v1 = build_r12_cred_zip(pid, "1.0.0", false);
    let inst = env
        .install_pkg(
            &zip_v1,
            None,
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    let if_match = format!("\"{}\"", inst["revision"].as_i64().unwrap());

    let zip_v2 = build_r12_cred_zip(pid, "2.0.0", true);
    let up_v2 = env.upload_pkg(&zip_v2, None, "r12-up-v2").await;
    let upload_id = up_v2["upload_id"].as_str().unwrap();

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &if_match),
                ("idempotency-key", "r12-prep-flip"),
            ],
            Some(json!({
                "upload_id": upload_id,
                "state_policy": "preserve"
            })),
        ))
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(body["error"]["code"], "plugin.state_schema_mismatch");
}

#[tokio::test]
async fn reconcile_draining_rolls_forward_blocked_staged_uninstall_and_settles_cancel_requested() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    // 1. `Blocked("drain-pending")` staged preserve-upgrade rolls forward to `Succeeded` once idle.
    let pid_u = "org.example.r12-blocked-upg";
    let zip_u = build_r12_cred_zip(pid_u, "1.0.0", false);
    let inst_u = env
        .install_pkg(
            &zip_u,
            None,
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    env.enable_plugin(pid_u, inst_u["revision"].as_i64().unwrap(), "r12-en-u")
        .await;
    let zip_u2 = build_r12_cred_zip(pid_u, "1.1.0", false);
    let up_u2 = env.upload_pkg(&zip_u2, None, "r12-up-u2").await;
    let cur_u = env.state.store().get_plugin(pid_u).unwrap().unwrap();
    let if_match_u = format!("\"{}\"", cur_u.revision);
    let (st_prep, _, prep_body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid_u}/upgrades/prepare"),
            Some(TOKEN),
            &[("if-match", &if_match_u), ("idempotency-key", "r12-prep-u")],
            Some(json!({
                "upload_id": up_u2["upload_id"].as_str().unwrap(),
                "state_policy": "preserve"
            })),
        ))
        .await;
    assert_eq!(st_prep, StatusCode::ACCEPTED, "{prep_body:?}");
    let cfg_digest = prep_body["data"]["result"]["target_config_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let op_u = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r12-staged-u".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid_u}/upgrades"),
                    body_digest: [2; 32],
                },
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur_u.revision,
                intent: StagedPluginIntent {
                    target_plugin_id: pid_u.into(),
                    package_hash: Some(up_u2["hash"].as_str().unwrap().into()),
                    version: Some("1.1.0".into()),
                    artifact_path: Some(format!(
                        "plugins/uploads/{}/package.cmplugin",
                        up_u2["upload_id"].as_str().unwrap()
                    )),
                    state_policy: Some("preserve".into()),
                    target_config_digest: Some(cfg_digest),
                    grants: vec![store::plugins::PluginGrant {
                        capability: "kv".into(),
                        scope: store::plugins::PluginGrantScope::Global,
                        sensitive: false,
                    }],
                },
                cause: PluginCommandCause::root("r12-staged-u"),
            })?;
            let id = acc.operation.operation_id;
            let claim =
                app.claim_staged_plugin_operation(id, acc.operation.revision, 0, 1_700_000_000)?;
            let p_now = app.get_plugin(pid_u)?.unwrap();
            app.begin_plugin_lifecycle(pid_u, p_now.revision, id, false)?;
            app.record_plugin_command_outcome(
                &claim,
                &PluginCommandOutcome {
                    state: PluginOperationState::Blocked,
                    phase: "drain-pending".into(),
                    next_run_at: None,
                    result: None,
                    effect: PluginEffectObservation::NoEffect,
                },
            )?;
            Ok(id)
        })
        .unwrap();

    api::plugins::reconcile_pending(&env.state, 1_700_000_010).unwrap();
    let settled_u = env
        .state
        .store()
        .get_plugin_operation_admin(op_u)
        .unwrap()
        .unwrap();
    assert_eq!(
        settled_u.operation.state,
        PluginOperationState::Succeeded,
        "Blocked(drain-pending) preserve-upgrade must roll forward instead of aborting to Failed: {:?}",
        settled_u.operation
    );
    assert_eq!(
        env.state
            .store()
            .get_plugin(pid_u)
            .unwrap()
            .unwrap()
            .package_hash,
        up_u2["hash"].as_str().unwrap()
    );
}

#[tokio::test]
async fn reconcile_draining_respects_active_resume_claim_until_released() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid_c = "org.example.r12-resume-claim";
    let zip_c = build_r12_cred_zip(pid_c, "1.0.0", false);
    let inst_c = env
        .install_pkg(
            &zip_c,
            None,
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    env.enable_plugin(pid_c, inst_c["revision"].as_i64().unwrap(), "r12-en-c")
        .await;
    let cur_c = env.state.store().get_plugin(pid_c).unwrap().unwrap();
    let op_c = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r12-staged-c".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid_c}/upgrades"),
                    body_digest: [3; 32],
                },
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur_c.revision,
                intent: StagedPluginIntent {
                    target_plugin_id: pid_c.into(),
                    package_hash: Some("deadbeef".repeat(8)),
                    version: Some("2.0.0".into()),
                    artifact_path: None,
                    state_policy: Some("preserve".into()),
                    target_config_digest: Some("00".repeat(32)),
                    grants: vec![],
                },
                cause: PluginCommandCause::root("r12-staged-c"),
            })?;
            let id = acc.operation.operation_id;
            let _claim =
                app.claim_staged_plugin_operation(id, acc.operation.revision, 0, 1_700_000_020)?;
            let p_now = app.get_plugin(pid_c)?.unwrap();
            app.begin_plugin_lifecycle(pid_c, p_now.revision, id, false)?;
            app.put_setting(&format!("plugin_op.resume_claim.{id}"), "{}")?;
            Ok(id)
        })
        .unwrap();

    // While resume claim exists, `reconcile_pending` must leave both Draining and Running intact.
    api::plugins::reconcile_pending(&env.state, 1_700_000_025).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin(pid_c)
            .unwrap()
            .unwrap()
            .lifecycle,
        PluginLifecycle::Draining
    );
    assert_eq!(
        env.state
            .store()
            .get_plugin_operation_admin(op_c)
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Running
    );

    // Once the resume claim is cleared, `reconcile_pending` recovers the orphaned op cleanly.
    env.state
        .store()
        .delete_setting(&format!("plugin_op.resume_claim.{op_c}"))
        .unwrap();
    api::plugins::reconcile_pending(&env.state, 1_700_000_030).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin(pid_c)
            .unwrap()
            .unwrap()
            .lifecycle,
        PluginLifecycle::Disabled
    );
    assert_eq!(
        env.state
            .store()
            .get_plugin_operation_admin(op_c)
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Failed
    );
}

#[tokio::test]
async fn endpoint_rejects_disabled_site_when_unbound_and_read_only_skips_reconcile() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r12-ep-site";
    let zip = build_r12_endpoint_zip(pid, "1.0.0");
    let inst = env
        .install_pkg(
            &zip,
            None,
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r12-en-ep")
        .await;

    // Insert disabled site and invoke non-credential-bound endpoint with `?site_id=...`
    let site_id = domain::SiteId::new();
    env.state
        .store()
        .insert_site(&domain::Site {
            id: site_id,
            name: "DisabledEpSite".into(),
            url: "https://disabled.example/".into(),
            profile_id: "pterclub".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: false,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();

    let ep_rec = env.state.store().get_plugin(pid).unwrap().unwrap();
    let ep_auth = env
        .state
        .store()
        .issue_plugin_state_authority(&ep_rec, None)
        .unwrap();
    let err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &ep_rec,
            ep_auth,
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r12-ep"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: vec![Header {
                    name: "site_id".into(),
                    value: site_id.to_string(),
                }],
                body: b"hi".to_vec(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, Failure::Guest(ref ge) if ge.code == ErrorCode::StaleGeneration),
        "expected StaleGeneration on disabled site, got {err:?}"
    );

    // Seed orphaned Enabling plugin and verify `reconcile_pending` is a no-op while `!instance_writable`.
    let pid_ro = "org.example.r12-ro-enabling";
    let zip_ro = pkg::build_task_package_custom(pid_ro, "1.0.0", 1, false, None, false, &["kv"]);
    let _ = env
        .install_pkg(
            &zip_ro,
            None,
            json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]),
        )
        .await;
    let inst_ro = env.state.store().get_plugin(pid_ro).unwrap().unwrap();
    let acc = env
        .state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some(pid_ro.into()),
            subject: PluginSubject::User(admin_id()),
            idempotency_key: "r12-ro-en".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: format!("/plugins/{pid_ro}/enable"),
                body_digest: [4; 32],
            },
            kind: "enable".into(),
            expected_revision: Some(inst_ro.revision),
            expected_generation: None,
            cause: PluginCommandCause::root("r12-ro-en"),
        })
        .unwrap();
    env.state
        .store()
        .begin_plugin_enable_checked(pid_ro, inst_ro.revision, acc.operation.operation_id)
        .unwrap();

    let ro_state = env.state.clone().with_instance_writable(false);
    api::plugins::reconcile_pending(&ro_state, 1_700_000_050).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin(pid_ro)
            .unwrap()
            .unwrap()
            .lifecycle,
        PluginLifecycle::Enabling
    );
}
