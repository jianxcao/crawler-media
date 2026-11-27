#![allow(dead_code)]

use api::{ApiState, Store};
use axum::http::StatusCode;
use domain::{DownloaderId, SiteId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycle, PluginLifecycleObservation, PluginRecord,
};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, RecordDownloadOwnershipInput, UpsertBoostTaskFactInput,
};
use uuid::Uuid;

use super::{
    HttpEnv, TOKEN, assert_ok,
    pkg::{TestSigningKey, build_stored_zip, task_wasm},
    req_json,
};

pub const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ui_bundle(remote_name: &str) -> (Vec<u8>, Vec<u8>) {
    let js = format!("export const remote = '{remote_name}';\n").into_bytes();
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(&js),
            "size": js.len(),
            "content_type": "application/javascript"
        }]
    }))
    .unwrap();
    (js, asset_manifest)
}

fn ui_toml(remote_name: &str, caps: &[&str]) -> String {
    let caps_json = serde_json::to_string(caps).unwrap();
    format!(
        r#"
[ui]
kind = "federation-react"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
remote_name = "{remote_name}"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
capabilities = {caps_json}

[[ui.surfaces]]
kind = "app"
id = "app"
expose = "./App"
title = "App"

[[ui.surfaces]]
kind = "config"
id = "settings"
expose = "./Config"
title = "Settings"

[[ui.surfaces]]
kind = "dashboard"
id = "overview"
expose = "./Dashboard"
title = "Overview"

[[ui.surfaces]]
kind = "page"
id = "main"
expose = "./Page"
title = "Main"
"#
    )
}

pub fn cm_checkin_pkg_for_id(
    plugin_id: &str,
    with_ui: bool,
    declare_browser_maint: bool,
    extra_perms: &[&str],
) -> Vec<u8> {
    let mut perms = vec![
        "kv",
        "blob",
        "read:sites",
        "command:site.maintenance",
        "command:site.check-in",
        "command:site.verify-session",
    ];
    if declare_browser_maint {
        perms.push("browser:maintenance");
    }
    for p in extra_perms {
        if !perms.contains(p) {
            perms.push(p);
        }
    }
    let perms_json = serde_json::to_string(&perms).unwrap();
    let ui_block = if with_ui {
        let mut ui_caps: Vec<&str> = perms
            .iter()
            .copied()
            .filter(|c| c.starts_with("read:") || c.starts_with("command:"))
            .collect();
        ui_caps.push("task:run:check-in");
        ui_toml("cmCheckinRemote", &ui_caps)
    } else {
        String::new()
    };
    let cred_line = if perms.iter().any(|c| {
        *c == "http:site-auth"
            || *c == "maintenance:provide"
            || c.starts_with("browser:")
            || c.starts_with("indexer:")
    }) {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Site Check-in"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["crawler-media"]
description = "Test check-in package"
config_schema = "schemas/config.schema.json"
state_schema = 1
{cred_line}
[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = {perms_json}

[[components.tasks]]
id = "check-in"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 86400
{ui_block}
"#
    );
    let cfg_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "site_id": { "type": "string" },
            "sites": { "type": "array", "items": { "type": "string" } },
            "interval_secs": { "type": "integer", "minimum": 60, "default": 86400 },
            "transport": { "type": "string", "enum": ["http", "browser", "auto"], "default": "auto" }
        }
    })
    .to_string();
    let task_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "site_id": { "type": "string" },
            "action": { "type": "string" }
        }
    })
    .to_string();

    let (js, asset_manifest) = ui_bundle("cmCheckinRemote");
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", task_wasm()),
    ];
    if with_ui {
        entries.push(("ui/asset-manifest.json", &asset_manifest));
        entries.push(("ui/assets/remoteEntry.js", &js));
    }
    build_stored_zip(&entries)
}

fn cm_boost_schemas() -> (String, String) {
    let cfg_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "downloader_id": { "type": "string" },
            "sites": { "type": "array", "items": { "type": "string" } },
            "budget_bytes": { "type": "integer", "minimum": 0 },
            "max_active": { "type": "integer", "minimum": 0 },
            "max_adds_per_round": { "type": "integer", "minimum": 0 },
            "min_free_disk_bytes": { "type": "integer", "minimum": 0 },
            "hold_days": { "type": "integer", "minimum": 0 },
            "dry_run": { "type": "boolean" },
            "auto_cleanup_files": { "type": "boolean" },
            "enforce_budget_on_all": { "type": "boolean" },
            "mode": { "type": "string" }
        }
    })
    .to_string();
    let task_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "site_id": { "type": "string" },
            "downloader_id": { "type": "string" },
            "dry_run": { "type": "boolean" }
        }
    })
    .to_string();
    (cfg_schema, task_schema)
}

pub fn cm_boost_pkg(with_ui: bool, with_hook: bool) -> Vec<u8> {
    let perms = [
        "kv",
        "blob",
        "read:sites",
        "read:boost",
        "read:downloads",
        "command:boost.run",
        "command:indexer.search",
        "command:indexer.rss",
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
        "command:schedule.set",
        "command:schedule.disable",
    ];
    let perms_json = serde_json::to_string(&perms).unwrap();
    let hook_block = if with_hook {
        r#"
[[components]]
id = "policy"
entry = "components/policy.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = ["hooks:download.before"]

[[components.hooks]]
id = "download.before"
priority = 10
"#
    } else {
        ""
    };
    let ui_block = if with_ui {
        ui_toml(
            "cmBoostRemote",
            &[
                "read:boost",
                "read:sites",
                "read:downloads",
                "command:boost.run",
                "command:downloader.delete-files",
                "command:downloader.prepare-cleanup",
                "command:downloader.control",
                "command:downloader.delete-task",
                "command:downloader.add",
                "task:run:traffic-cycle",
            ],
        )
    } else {
        String::new()
    };
    let manifest = format!(
        r#"manifest_version = 1
id = "cm.boost"
name = "Brushflow Boost"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["crawler-media"]
description = "Test boost package"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = {perms_json}

[[components.tasks]]
id = "traffic-cycle"
input_schema = "schemas/task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
{hook_block}
{ui_block}
"#
    );
    let (cfg_schema, task_schema) = cm_boost_schemas();

    let (js, asset_manifest) = ui_bundle("cmBoostRemote");
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", task_wasm()),
    ];
    if with_hook {
        entries.push(("components/policy.wasm", POLICY_WASM));
    }
    if with_ui {
        entries.push(("ui/asset-manifest.json", &asset_manifest));
        entries.push(("ui/assets/remoteEntry.js", &js));
    }
    build_stored_zip(&entries)
}

pub fn default_cm_checkin_grants(include_browser: bool) -> Value {
    let mut caps = vec![
        "kv",
        "blob",
        "read:sites",
        "command:site.maintenance",
        "command:site.check-in",
        "command:site.verify-session",
    ];
    if include_browser {
        caps.push("browser:maintenance");
    }
    Value::Array(
        caps.into_iter()
            .map(|c| json!({ "capability": c, "scope": "Global", "sensitive": c.starts_with("browser:") }))
            .collect(),
    )
}

pub fn default_cm_boost_grants(include_hook: bool) -> Value {
    let mut caps = vec![
        "kv",
        "blob",
        "read:sites",
        "read:boost",
        "read:downloads",
        "command:boost.run",
        "command:indexer.search",
        "command:indexer.rss",
        "command:downloader.add",
        "command:downloader.control",
        "command:downloader.delete-task",
        "command:downloader.prepare-cleanup",
        "command:downloader.delete-files",
        "command:schedule.set",
        "command:schedule.disable",
    ];
    if include_hook {
        caps.push("hooks:download.before");
    }
    Value::Array(
        caps.into_iter()
            .map(|c| json!({ "capability": c, "scope": "Global", "sensitive": false }))
            .collect(),
    )
}

pub async fn grant_ui_trust(env: &HttpEnv, pid: &str) -> Value {
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let rev = prep["trust_revision"].as_i64().unwrap_or(0);
    let reauth = env.reauth("plugin-ui-trust").await;
    let idem = format!("uit-{}", Uuid::new_v4());
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &idem),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK)
}

pub async fn install_signed_and_enable(
    env: &HttpEnv,
    seed: u64,
    pkg: &[u8],
    grants: Value,
    trust_ui: bool,
) -> Value {
    let key = TestSigningKey::generate(seed);
    env.pin_trust_key(&key, &format!("test-key-{seed}")).await;
    let sig = key.sign_package(pkg);
    let detail = env.install_pkg(pkg, Some(&sig), grants).await;
    let pid = detail["id"].as_str().unwrap().to_string();
    let rev = detail["revision"].as_i64().unwrap();
    env.enable_plugin(&pid, rev, &format!("en-{pid}-{}", Uuid::new_v4()))
        .await;
    if trust_ui {
        grant_ui_trust(env, &pid).await;
    }
    let (st, _, body) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK)
}

pub fn cm_checkin_pkg(with_ui: bool, declare_browser_maint: bool, extra_perms: &[&str]) -> Vec<u8> {
    cm_checkin_pkg_for_id("cm.checkin", with_ui, declare_browser_maint, extra_perms)
}

pub async fn install_cm_checkin_with_id(
    env: &HttpEnv,
    plugin_id: &str,
    seed: u64,
    with_ui: bool,
    declare_browser_maint: bool,
    grant_browser_maint: bool,
    trust_ui: bool,
) -> Value {
    let pkg = cm_checkin_pkg_for_id(plugin_id, with_ui, declare_browser_maint, &[]);
    let grants = default_cm_checkin_grants(grant_browser_maint);
    install_signed_and_enable(env, seed, &pkg, grants, trust_ui).await
}

pub async fn install_cm_checkin(
    env: &HttpEnv,
    with_ui: bool,
    declare_browser_maint: bool,
    grant_browser_maint: bool,
    trust_ui: bool,
) -> Value {
    install_cm_checkin_with_id(
        env,
        "cm.checkin",
        9101,
        with_ui,
        declare_browser_maint,
        grant_browser_maint,
        trust_ui,
    )
    .await
}

pub async fn install_cm_boost(
    env: &HttpEnv,
    with_ui: bool,
    with_hook: bool,
    trust_ui: bool,
) -> Value {
    let pkg = cm_boost_pkg(with_ui, with_hook);
    let grants = default_cm_boost_grants(with_hook);
    install_signed_and_enable(env, 9102, &pkg, grants, trust_ui).await
}

pub fn set_plugin_lifecycle_store(
    store: &Store,
    pid: &str,
    rev: i64,
    enable: bool,
) -> PluginRecord {
    let trans = store
        .begin_plugin_lifecycle(pid, rev, Uuid::new_v4(), enable)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: pid.into(),
            operation_id: trans.active_operation_id.unwrap(),
            revision: trans.revision,
            generation: trans.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

pub fn replace_grants_and_enable_store(store: &Store, pid: &str, grants: &[PluginGrant]) {
    let mut p = store.get_plugin(pid).unwrap().unwrap();
    if p.lifecycle == PluginLifecycle::Enabled {
        p = set_plugin_lifecycle_store(store, pid, p.revision, false);
    }
    let g = store
        .replace_plugin_grants(pid, &p.package_hash, p.revision, grants)
        .unwrap();
    set_plugin_lifecycle_store(store, pid, g.revision, true);
}

pub fn save_plugin_config_store(store: &Store, plugin_id: &str, values: Value) {
    let mut cur = store.get_plugin(plugin_id).unwrap().unwrap();
    let schema = store
        .get_plugin_config_summary(plugin_id)
        .unwrap()
        .config
        .map(|c| c.schema_revision)
        .unwrap_or_else(|| "1".into());
    if cur.lifecycle == PluginLifecycle::Enabled {
        cur = set_plugin_lifecycle_store(store, plugin_id, cur.revision, false);
    }
    let saved = store
        .save_plugin_config(plugin_id, cur.revision, &schema, &values)
        .unwrap();
    set_plugin_lifecycle_store(store, plugin_id, saved.revision, true);
}

pub fn save_plugin_config_direct(state: &ApiState, plugin_id: &str, values: Value) {
    save_plugin_config_store(&state.store(), plugin_id, values);
}

#[allow(clippy::too_many_arguments)]
pub fn seed_cm_boost_task(
    store: &Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    tag: &str,
    hash: &str,
    state_val: BoostTaskState,
    occupied_bytes: i64,
    retained_bytes: i64,
    hr_active: Option<bool>,
    hold_until: Option<i64>,
) -> String {
    let op_id = format!("op-{tag}");
    let now = 1_700_000_000i64;
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{tag}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "cm.boost".into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            bytes_digest: Some(format!("dig-{tag}")),
            verified_bytes: Some(occupied_bytes.max(retained_bytes)),
            request_digest: format!("req-{tag}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.into(),
            info_hash: Some(hash.into()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some("cm.boost".into()),
            subject: "cm.boost".into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{tag}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(occupied_bytes.max(retained_bytes)),
            bytes_digest: Some(format!("dig-{tag}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: format!("tf-{tag}"),
            plugin_id: "cm.boost".into(),
            quota_key: format!("cm.boost:{dl_id}"),
            downloader_id: dl_id,
            site_id: Some(site_id),
            ownership_tag: tag.into(),
            info_hash: hash.into(),
            state: state_val,
            occupied_bytes,
            retained_bytes,
            hr_active,
            hr_until: None,
            hold_until,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap()
        .task_fact_id
}

pub fn boost_scope_site(site_id: SiteId) -> PluginGrantScope {
    PluginGrantScope::Site(site_id)
}
