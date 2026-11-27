use std::time::{Duration, Instant};

use api::plugins::entrypoints::recover_plugin_startup_for_test;
use axum::http::StatusCode;
use serde_json::json;
use store::plugins::{
    AcceptStagedPluginOperation, PluginCommandCause, PluginLifecycle, PluginOperationState,
    PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};

use crate::support::plugin_http_support::{HttpEnv, admin_id, assert_err, assert_ok, pkg};
use crate::{get_json, http_canon_req, post_plain, rotate_site_credentials, send_write};

fn dual_secret_schema(require_both_top_level: bool) -> (String, String) {
    let req = if require_both_top_level {
        r#"["mode", "auth_a", "auth_b"]"#
    } else {
        r#"["mode"]"#
    };
    let raw = format!(
        r#"{{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "required": {req},
  "properties": {{
    "mode": {{ "type": "string", "default": "ok" }},
    "auth_a": {{
      "type": "object",
      "additionalProperties": false,
      "required": ["secret_a"],
      "properties": {{
        "secret_a": {{ "type": "string", "minLength": 4, "writeOnly": true }}
      }}
    }},
    "auth_b": {{
      "type": "object",
      "additionalProperties": false,
      "required": ["secret_b"],
      "properties": {{
        "secret_b": {{ "type": "string", "minLength": 4, "writeOnly": true }}
      }}
    }}
  }}
}}"#
    );
    let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let rev = plugin_package::validate_config_schema(&val)
        .unwrap()
        .schema_revision()
        .to_string();
    (raw, rev)
}

fn build_custom_schema_pkg(
    id: &str,
    version: &str,
    schema_json: &str,
    credential_bound: bool,
    comp_count: usize,
) -> Vec<u8> {
    let cred_line = if credential_bound {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    let mut comps_toml = String::new();
    let mut entries: Vec<(String, &[u8])> = Vec::new();
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    for idx in 0..comp_count {
        let entry_path = format!("components/w{idx}.wasm");
        comps_toml.push_str(&format!(
            r#"
[[components]]
id = "worker-{idx}"
entry = "{entry_path}"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv"]

[[components.tasks]]
id = "sync-{idx}"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
        ));
        entries.push((entry_path, pkg::task_wasm()));
    }
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R6 Custom Schema {version}"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Per-scope secret and enable TOCTOU test"
config_schema = "schemas/config.schema.json"
state_schema = 1
{cred_line}{comps_toml}"#
    );
    let mut zip_refs: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", schema_json.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
    ];
    for (p, b) in &entries {
        zip_refs.push((p.as_str(), *b));
    }
    pkg::build_stored_zip(&zip_refs)
}

#[tokio::test]
async fn slice_d_r6_item3a_per_scope_secrets_reject_multisite_union_while_preserving_get_redaction()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_1 = env.insert_site("R6-SplitSite1");
    let site_2 = env.insert_site("R6-SplitSite2");

    let pid = "org.example.r6-split-sec";
    let (schema_v1, srev_v1) = dual_secret_schema(false);
    let (schema_v2, srev_v2) = dual_secret_schema(true);
    let z1 = build_custom_schema_pkg(pid, "1.0.0", &schema_v1, true, 1);
    let z2 = build_custom_schema_pkg(pid, "1.1.0", &schema_v2, true, 1);
    let rev0 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let cfg_path = format!("/api/v1/plugins/{pid}/config");
    let (st1, b1) = send_write(
        &env,
        "PUT",
        &cfg_path,
        rev0,
        "r6-s1-only-a",
        json!({
            "schema_revision": srev_v1,
            "values": { "mode": "s1", "auth_a": {} },
            "secrets": { "auth_a.secret_a": { "action": "replace", "value": "only-on-s1" } },
            "site_id": site_1,
        }),
    )
    .await;
    let rev1 = assert_ok(st1, &b1, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let (st2, b2) = send_write(
        &env,
        "PUT",
        &cfg_path,
        rev1,
        "r6-s2-only-b",
        json!({
            "schema_revision": srev_v1,
            "values": { "mode": "s2", "auth_b": {} },
            "secrets": { "auth_b.secret_b": { "action": "replace", "value": "only-on-s2" } },
            "site_id": site_2,
        }),
    )
    .await;
    let rev2 = assert_ok(st2, &b2, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    // `get_config` retains union across live scopes for redaction display.
    let (st_get, b_get) = get_json(&env, &cfg_path).await;
    let got = assert_ok(st_get, &b_get, StatusCode::OK);
    assert_eq!(got["secrets"]["auth_a.secret_a"]["configured"], true);
    assert_eq!(got["secrets"]["auth_b.secret_b"]["configured"], true);

    assert_split_scopes_rejected(&env, pid, &srev_v1, &srev_v2, &z2, rev2).await;

    // Completing BOTH secrets on `site_1` allows validation, public update, and enable!
    let (st_both, b_both) = send_write(
        &env,
        "PUT",
        &cfg_path,
        rev2,
        "r6-s1-both",
        json!({
            "schema_revision": srev_v1,
            "values": { "mode": "both", "auth_a": {}, "auth_b": {} },
            "secrets": { "auth_b.secret_b": { "action": "replace", "value": "also-on-s1" } },
            "site_id": site_1,
        }),
    )
    .await;
    let rev3 = assert_ok(st_both, &b_both, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    let (st_v_ok, b_v_ok) = post_plain(
        &env,
        format!("/api/v1/plugins/{pid}/config/validate"),
        json!({
            "schema_revision": srev_v1,
            "values": { "mode": "both", "auth_a": {}, "auth_b": {} },
            "secrets": {}
        }),
    )
    .await;
    assert_ok(st_v_ok, &b_v_ok, StatusCode::OK);
    env.enable_plugin(pid, rev3, "r6-s1-en-ok").await;
}

async fn assert_split_scopes_rejected(
    env: &HttpEnv,
    pid: &str,
    srev_v1: &str,
    srev_v2: &str,
    z2: &[u8],
    rev2: i64,
) {
    let both_vals = json!({ "mode": "both", "auth_a": {}, "auth_b": {} });
    let (st_val, b_val) = post_plain(
        env,
        format!("/api/v1/plugins/{pid}/config/validate"),
        json!({ "schema_revision": srev_v1, "values": both_vals, "secrets": {} }),
    )
    .await;
    assert_err(
        st_val,
        &b_val,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );

    let (st_pub, b_pub) = send_write(
        env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev2,
        "r6-split-pub",
        json!({ "schema_revision": srev_v1, "values": both_vals, "secrets": {} }),
    )
    .await;
    assert_err(
        st_pub,
        &b_pub,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );

    let up2 = env.upload_pkg(z2, None, "r6-split-up2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let (st_pu, b_pu) = send_write(
        env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev2,
        "r6-split-pu",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "reset",
            "target_config": { "schema_revision": srev_v2, "values": both_vals, "secrets": {} }
        }),
    )
    .await;
    assert_err(
        st_pu,
        &b_pu,
        StatusCode::BAD_REQUEST,
        "plugin.config_invalid",
    );
}

#[tokio::test]
async fn slice_d_r6_item3b_implicit_keep_upgrade_binds_site_obs_and_rejects_site_a_rotation_even_with_live_site_b()
 {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();
    let site_a = env.insert_site("R6-ImpKeepA");
    let site_b = env.insert_site("R6-ImpKeepB");

    let pid = "org.example.r6-imp-keep";
    let z1 = pkg::build_task_package_custom(pid, "1.0.0", 1, true, None, true, &["kv"]);
    let z2 = pkg::build_task_package_custom(pid, "1.1.0", 1, true, None, true, &["kv"]);
    let srev = pkg::schema_rev(true, None);
    let mut rev = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    for (idem, sid) in [("r6-ik-a", site_a), ("r6-ik-b", site_b)] {
        let (st, b) = send_write(
            &env,
            "PUT",
            format!("/api/v1/plugins/{pid}/config"),
            rev,
            idem,
            json!({
                "schema_revision": srev,
                "values": { "mode": "v1" },
                "secrets": { "api_secret": { "action": "replace", "value": "shared-valid" } },
                "site_id": sid,
            }),
        )
        .await;
        rev = assert_ok(st, &b, StatusCode::OK)["revision"]
            .as_i64()
            .unwrap();
    }

    let up2 = env.upload_pkg(&z2, None, "r6-ik-up2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let (st_prep, b_prep) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev,
        "r6-ik-prep",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "site_id": site_a,
            "state_policy": "preserve",
            "target_config": {
                "schema_revision": srev,
                "values": { "mode": "v2" },
                "secrets": {}
            }
        }),
    )
    .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED)["result"].clone();

    // Rotate ONLY `site_a`; `site_b` still has a live `api_secret`!
    rotate_site_credentials(&store, site_a);

    let (st_ex, b_ex) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrade"),
        rev,
        "r6-ik-exec",
        json!({
            "preparation_id": prep["preparation_id"],
            "consent_token": prep["consent_token"],
            "grants": [],
            "state_policy": "preserve",
            "target_config_digest": prep["target_config_digest"],
        }),
    )
    .await;
    assert_err(st_ex, &b_ex, StatusCode::CONFLICT, "plugin.consent_stale");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_d_r6_item3c_rollforward_rechecks_secret_scopes_in_write_tx() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();
    let site_rf = env.insert_site("R6-RfRaceSite");

    let pid = "org.example.r6-rf-race";
    let schema = pkg::sample_schema(true, None);
    let srev = pkg::schema_rev(true, None);
    let z1 = build_custom_schema_pkg(pid, "1.0.0", &schema, true, 1);
    let z2 = build_custom_schema_pkg(pid, "1.1.0", &schema, true, 6);
    let rev0 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let (st_c, b_c) = send_write(
        &env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev0,
        "r6-rf-cfg",
        json!({
            "schema_revision": srev,
            "values": { "mode": "rf" },
            "secrets": { "api_secret": { "action": "replace", "value": "rf-live-sec" } },
            "site_id": site_rf,
        }),
    )
    .await;
    let rev1 = assert_ok(st_c, &b_c, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let up2 = env.upload_pkg(&z2, None, "r6-rf-up2").await;
    let uid2 = up2["upload_id"].as_str().unwrap();
    let hash2 = up2["hash"].as_str().unwrap().to_string();
    let (st_p, b_p) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev1,
        "r6-rf-prep",
        json!({
            "version": "1.1.0",
            "upload_id": uid2,
            "state_policy": "preserve",
            "target_config": { "schema_revision": srev, "values": { "mode": "rf" }, "secrets": {} }
        }),
    )
    .await;
    let digest = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"]["target_config_digest"]
        .as_str()
        .unwrap()
        .to_string();

    let rel_art = format!("plugins/uploads/{uid2}/package.cmplugin");
    let op_id = store
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r6-rf-op".into(),
                request: http_canon_req("POST", &format!("/plugins/{pid}/upgrade"), &json!({})),
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: rev1,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(hash2.clone()),
                    version: Some("1.1.0".into()),
                    artifact_path: Some(rel_art),
                    state_policy: Some("preserve".into()),
                    target_config_digest: Some(digest),
                    grants: Vec::new(),
                },
                cause: PluginCommandCause::root("r6-rf"),
            })?;
            let _ = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )?;
            Ok(acc.operation.operation_id)
        })
        .unwrap();

    // `packages/{hash2}` is materialized strictly AFTER pre-tx `verify_preserve_rollforward_config`
    // and BEFORE `validate_installed_components` (6 WASMs) + `app_write_service`.
    let pkg_dir = store.data_dir().join("plugins/packages").join(&hash2);
    let store_bg = store.clone();
    let rotator = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !pkg_dir.exists() {
            std::thread::sleep(Duration::from_micros(300));
        }
        assert!(
            pkg_dir.exists(),
            "expected rollforward materialization to start"
        );
        rotate_site_credentials(&store_bg, site_rf);
    });

    recover_plugin_startup_for_test(&env.state).unwrap();
    rotator.join().unwrap();

    let after = store.get_plugin(pid).unwrap().unwrap();
    assert_ne!(
        after.package_hash, hash2,
        "rollforward must abort when secret scope rotates mid-validation"
    );
    let admin_op = store.get_plugin_operation_admin(op_id).unwrap().unwrap();
    assert_eq!(admin_op.operation.state, PluginOperationState::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_d_r6_item3d_enable_plugin_checks_object_grants_and_rechecks_config_in_finalize_tx() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();
    let site_del = env.insert_site("R6-EnDelSite");
    let dl_race = env.insert_downloader("R6-EnRaceDl");

    let obj_schema = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "required": ["site_id", "downloader_id", "api_secret"],
  "properties": {
    "site_id": { "type": "string", "x-ui": { "widget": "site" } },
    "downloader_id": { "type": "string", "x-ui": { "widget": "downloader" } },
    "api_secret": { "type": "string", "minLength": 4, "writeOnly": true }
  }
}"#;
    let srev = plugin_package::validate_config_schema(&serde_json::from_str(obj_schema).unwrap())
        .unwrap()
        .schema_revision()
        .to_string();

    let pid = "org.example.r6-en-toctou";
    let zip = build_custom_schema_pkg(pid, "1.0.0", obj_schema, true, 6);
    let rev0 = env.install_pkg(&zip, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let (st_cfg, b_cfg) = send_write(
        &env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev0,
        "r6-en-cfg1",
        json!({
            "schema_revision": srev,
            "values": { "site_id": site_del, "downloader_id": dl_race },
            "secrets": { "api_secret": { "action": "replace", "value": "en-secret" } },
            "site_id": site_del,
        }),
    )
    .await;
    let rev1 = assert_ok(st_cfg, &b_cfg, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    // 1. Delete referenced downloader BEFORE `enable_plugin`: pre-check must reject!
    let dl_pre = env.insert_downloader("R6-PreDelDl");
    let (st_cfg2, b_cfg2) = send_write(
        &env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        rev1,
        "r6-en-cfg2",
        json!({
            "schema_revision": srev,
            "values": { "site_id": site_del, "downloader_id": dl_pre },
            "secrets": {},
        }),
    )
    .await;
    let rev2 = assert_ok(st_cfg2, &b_cfg2, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    store.delete_downloader(dl_pre).unwrap();

    let (st_en1, b_en1) = send_write(
        &env,
        "POST",
        format!("/api/v1/plugins/{pid}/enable"),
        rev2,
        "r6-en-predel",
        json!({}),
    )
    .await;
    let op1 = assert_ok(st_en1, &b_en1, StatusCode::ACCEPTED);
    assert_eq!(op1["state"], "failed", "op1={op1}");
    assert_eq!(
        store.get_plugin(pid).unwrap().unwrap().lifecycle,
        PluginLifecycle::Disabled
    );

    assert_enable_in_tx_rotation_race(&env, pid, &srev, site_del, dl_race).await;
}

async fn assert_enable_in_tx_rotation_race(
    env: &HttpEnv,
    pid: &str,
    srev: &str,
    site_del: domain::SiteId,
    dl_race: domain::DownloaderId,
) {
    let store = env.state.store();
    let cur_rev = env.get_plugin_rev(pid).await;
    let (st_cfg3, b_cfg3) = send_write(
        env,
        "PUT",
        format!("/api/v1/plugins/{pid}/config"),
        cur_rev,
        "r6-en-cfg3",
        json!({
            "schema_revision": srev,
            "values": { "site_id": site_del, "downloader_id": dl_race },
            "secrets": {},
        }),
    )
    .await;
    let rev3 = assert_ok(st_cfg3, &b_cfg3, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    let pid_bg = pid.to_string();
    let store_bg = store.clone();
    let rotator = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if store_bg
                .get_plugin(&pid_bg)
                .unwrap()
                .is_some_and(|p| p.lifecycle == PluginLifecycle::Enabling)
            {
                std::thread::sleep(Duration::from_millis(5));
                rotate_site_credentials(&store_bg, site_del);
                return;
            }
            std::thread::sleep(Duration::from_micros(300));
        }
        panic!("plugin never entered Enabling");
    });

    let (st_en2, b_en2) = send_write(
        env,
        "POST",
        format!("/api/v1/plugins/{pid}/enable"),
        rev3,
        "r6-en-race-rot",
        json!({}),
    )
    .await;
    rotator.join().unwrap();
    let op2 = assert_ok(st_en2, &b_en2, StatusCode::ACCEPTED);
    assert_eq!(op2["state"], "failed", "op2={op2}");
    assert_eq!(op2["phase"], "config-invalid");
    assert_eq!(
        store.get_plugin(pid).unwrap().unwrap().lifecycle,
        PluginLifecycle::Disabled
    );
}
