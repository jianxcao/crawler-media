use std::time::Duration;

use axum::http::StatusCode;
use plugin_api::{EndpointRequest, ErrorCode, GuestRole, Header};
use plugin_host::Invocation;
use serde_json::json;
use store::plugins::{PluginCommandCause, PluginLifecycle, PluginSubject};

use crate::send_write;
use crate::support::plugin_http_support::{HttpEnv, admin_id, assert_err, assert_ok, pkg};

const EP_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/endpoint_fixture.wasm");

fn build_ep_zip(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R6 Drain Fence {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Post-commit close_dispatch test"
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

fn assert_endpoint_and_ports_open(env: &HttpEnv, pid: &str, expected_gen: i64) {
    let store = env.state.store();
    let cur = store.get_plugin(pid).unwrap().unwrap();
    assert_eq!(cur.lifecycle, PluginLifecycle::Enabled);
    assert_eq!(cur.generation, expected_gen);

    let auth = store.issue_plugin_state_authority(&cur, None).unwrap();
    let resp = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &cur,
            auth,
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r6-item1-ep"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"still-alive".to_vec(),
            },
        )
        .expect("enabled plugin dispatch must remain open when Draining tx did not commit");
    assert_eq!(resp.status, 200);

    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = u64::try_from(expected_gen).unwrap();
    inv.role = GuestRole::Endpoint;
    let guard = ports
        .begin_invocation(&inv)
        .expect("HostPorts::begin_invocation must stay open at current generation");
    drop(guard);
}

#[tokio::test]
async fn slice_d_r6_item1_failed_drain_tx_does_not_close_dispatch_until_draining_commits() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r6-drain-fence";
    let z1 = build_ep_zip(pid, "1.0.0");
    let z2 = build_ep_zip(pid, "1.1.0");
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let inst = env.install_pkg(&z1, None, grants.clone()).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r6-df-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let gen0 = enabled.generation;
    let rev_en = enabled.revision;
    assert_endpoint_and_ports_open(&env, pid, gen0);

    // 1. Writable `revoke_permissions` failing inside `run_app_write_tx` on duplicate grant.
    let dup_grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "kv", "scope": "Global", "sensitive": false }
    ]);
    let (st_rev_bad, b_rev_bad) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/permissions/revoke"),
        rev_en,
        "r6-rev-dup",
        json!({ "grants": dup_grants, "reason": "bad-dup" }),
    )
    .await;
    assert_eq!(st_rev_bad, StatusCode::BAD_REQUEST, "body={b_rev_bad}");
    assert_endpoint_and_ports_open(&env, pid, gen0);

    // Prepare a valid upgrade while writable, then test read-only write-tx failures on all 4 ops.
    let up2 = env.upload_pkg(&z2, None, "r6-df-up2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let srev = pkg::schema_rev(false, None);
    let (st_prep, b_prep) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev_en,
        "r6-df-prep",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "preserve",
            "target_config": { "schema_revision": srev, "values": { "mode": "ok" }, "secrets": {} }
        }),
    )
    .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED)["result"].clone();

    assert_readonly_failures_keep_dispatch_open(&env, pid, rev_en, gen0, &grants, &prep).await;

    // Real disable commits `Draining` -> `Disabled` and closes old generation.
    env.disable_plugin(pid, rev_en, "r6-df-real-dis").await;
    let mut stale_inv = Invocation::new(pid, Duration::from_secs(5));
    stale_inv.generation = u64::try_from(gen0).unwrap();
    stale_inv.role = GuestRole::Endpoint;
    assert_eq!(
        env.state
            .plugins()
            .host_ports()
            .begin_invocation(&stale_inv)
            .err()
            .unwrap()
            .code,
        ErrorCode::StaleGeneration
    );
}

async fn assert_readonly_failures_keep_dispatch_open(
    env: &HttpEnv,
    pid: &str,
    rev_en: i64,
    gen0: i64,
    grants: &serde_json::Value,
    prep: &serde_json::Value,
) {
    let _ = env.state.clone().with_instance_writable(false);

    for (idem, path, body) in [
        (
            "r6-ro-dis",
            format!("/api/v1/plugins/{pid}/disable"),
            json!({ "reason": "ro" }),
        ),
        (
            "r6-ro-uninst",
            format!("/api/v1/plugins/{pid}/uninstall"),
            json!({ "remove_private_data": false }),
        ),
        (
            "r6-ro-upg",
            format!("/api/v1/plugins/{pid}/upgrade"),
            json!({
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
                "state_policy": "preserve",
                "target_config_digest": prep["target_config_digest"],
            }),
        ),
        (
            "r6-ro-rev",
            format!("/api/v1/plugins/{pid}/permissions/revoke"),
            json!({ "grants": grants, "reason": "ro" }),
        ),
    ] {
        let (st, b) = send_write(env, "POST", path, rev_en, idem, body).await;
        assert_err(st, &b, StatusCode::CONFLICT, "instance.read_only");
    }

    let _ = env.state.clone().with_instance_writable(true);
    assert_endpoint_and_ports_open(env, pid, gen0);
}
