#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use plugin_api::{CommandAction, CommandRequest, ErrorCode, GuestRole, MaintenanceAction};
use serde_json::{Value, json};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginOperationState,
    PluginStateKind, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use tower::ServiceExt;

use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};

fn build_task_zip(id: &str, perms: &[&str]) -> Vec<u8> {
    let manifest = pkg::task_manifest_with_options(id, "1.0.0", 1, false, perms);
    let cfg_schema = pkg::sample_schema(false, None);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", pkg::task_wasm()),
    ])
}

fn cmd_req(key: &str, action: CommandAction) -> CommandRequest {
    CommandRequest {
        action,
        idempotency_key: key.into(),
        expected_revision: None,
    }
}

#[tokio::test]
async fn p1_member_cookie_paired_with_admin_bearer_is_rejected_on_plugin_ui_assets() {
    let env = HttpEnv::new().await;
    let (st_ui, _, b_ui) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin/ui",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let entry_url = assert_ok(st_ui, &b_ui, StatusCode::OK)["entry_url"]
        .as_str()
        .unwrap()
        .to_string();

    let login = env
        .app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(json!({ "username": "admin", "password": ADMIN_PASS })),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let admin_cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let ok_req = Request::builder()
        .method("GET")
        .uri(&entry_url)
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        env.app.clone().oneshot(ok_req).await.unwrap().status(),
        StatusCode::OK
    );

    let mixed_req = Request::builder()
        .method("GET")
        .uri(&entry_url)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::COOKIE, format!("mc_session={}", env.member_token))
        .body(Body::empty())
        .unwrap();
    let mixed = env.app.clone().oneshot(mixed_req).await.unwrap();
    let st = mixed.status();
    let bytes = to_bytes(mixed.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_err(st, &body, StatusCode::FORBIDDEN, "auth.forbidden");
}

fn stage_running_uninstall(
    env: &HttpEnv,
    actor: domain::UserId,
    pid: &str,
    expected_revision: i64,
    state_policy: &str,
) -> uuid::Uuid {
    let acc = env
        .state
        .store()
        .begin_staged_plugin_operation(&AcceptStagedPluginOperation {
            subject: PluginSubject::User(actor),
            idempotency_key: format!("crash-uninst-{pid}"),
            request: CanonicalPluginRequest {
                method: "DELETE".into(),
                path: format!("/plugins/{pid}"),
                body_digest: [9; 32],
            },
            op_kind: StagedPluginOpKind::Uninstall,
            expected_revision,
            intent: StagedPluginIntent {
                target_plugin_id: pid.into(),
                package_hash: None,
                version: None,
                artifact_path: None,
                state_policy: Some(state_policy.into()),
                target_config_digest: None,
                grants: Vec::new(),
            },
            cause: PluginCommandCause::root("crash-uninst"),
        })
        .unwrap();
    env.state
        .store()
        .claim_staged_plugin_operation(acc.operation.operation_id, acc.operation.revision, 0, 10)
        .unwrap();
    acc.operation.operation_id
}

#[tokio::test]
async fn p1_staged_uninstall_crash_recovery_purges_private_kv_when_remove_private_data_true() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let actor = admin_id();
    let pid = "org.example.uninstrec";
    let pkg_bytes = build_task_zip(pid, &["kv"]);
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);

    let inst0 = env.install_pkg(&pkg_bytes, None, grants.clone()).await;
    let op0 = env
        .state
        .plugins()
        .uninstall_plugin(
            &env.state,
            pid,
            api::plugins::UninstallPluginInput {
                actor_user_id: actor,
                remove_private_data: true,
                expected_revision: inst0["revision"].as_i64().unwrap(),
                idempotency_key: "uninst-intent-1".into(),
                request: CanonicalPluginRequest {
                    method: "DELETE".into(),
                    path: format!("/plugins/{pid}"),
                    body_digest: [7; 32],
                },
            },
        )
        .await
        .unwrap();
    let saved0 = env
        .state
        .store()
        .get_plugin_operation_admin(op0.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved0
            .intent
            .as_ref()
            .and_then(|i| i.state_policy.as_deref()),
        Some("remove")
    );

    let inst1 = env.install_pkg(&pkg_bytes, None, grants.clone()).await;
    env.enable_plugin(pid, inst1["revision"].as_i64().unwrap(), "en-uninstrec-1")
        .await;
    let p_en = env.state.store().get_plugin(pid).unwrap().unwrap();
    let auth1 = env
        .state
        .store()
        .issue_plugin_state_authority(&p_en, None)
        .unwrap();
    env.state
        .store()
        .plugin_state_put(&auth1, PluginStateKind::Kv, "priv/token", b"secret-kv", 0)
        .unwrap();
    env.disable_plugin(pid, p_en.revision, "dis-uninstrec-1")
        .await;
    let p_dis = env.state.store().get_plugin(pid).unwrap().unwrap();
    let crash_op_id = stage_running_uninstall(&env, actor, pid, p_dis.revision, "remove");

    api::plugins::recover_startup(&env.state).unwrap();
    assert!(env.state.store().get_plugin(pid).unwrap().is_none());
    let rec_op = env
        .state
        .store()
        .get_plugin_operation_admin(crash_op_id)
        .unwrap()
        .unwrap();
    assert_eq!(rec_op.operation.state, PluginOperationState::Succeeded);

    let inst2 = env.install_pkg(&pkg_bytes, None, grants).await;
    env.enable_plugin(pid, inst2["revision"].as_i64().unwrap(), "en-uninstrec-2")
        .await;
    let p_en2 = env.state.store().get_plugin(pid).unwrap().unwrap();
    let auth2 = env
        .state
        .store()
        .issue_plugin_state_authority(&p_en2, None)
        .unwrap();
    assert!(
        env.state
            .store()
            .plugin_state_get(&auth2, PluginStateKind::Kv, "priv/token")
            .unwrap()
            .is_none(),
        "private KV rows must be purged on recover_startup when state_policy is remove"
    );
}

#[tokio::test]
async fn p1_site_bound_invocation_rejects_cross_site_and_global_targets_even_with_global_grant() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_a = env.insert_site("BoundSiteA");
    let site_b = env.insert_site("OtherSiteB");
    let pid = "org.example.cmdscope";
    let caps = [
        "command:site.check-in",
        "command:site.maintenance",
        "command:schedule.set",
    ];
    let grants = json!([
        { "capability": "command:site.check-in", "scope": "Global", "sensitive": false },
        { "capability": "command:site.maintenance", "scope": "Global", "sensitive": false },
        { "capability": "command:schedule.set", "scope": "Global", "sensitive": false },
    ]);
    let inst = env
        .install_pkg(&build_task_zip(pid, &caps), None, grants)
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-cmdscope-1")
        .await;

    let p_en = env.state.store().get_plugin(pid).unwrap().unwrap();
    let obs_a = env.state.store().observe_site(site_a).unwrap().unwrap();
    let ports = env.state.plugins().host_ports();
    let mut inv = plugin_host::Invocation::new(pid, Duration::from_secs(5));
    inv.generation = u64::try_from(p_en.generation).unwrap();
    inv.role = GuestRole::Task;
    inv.site_id = Some(site_a.to_string());
    inv.credential_epoch = Some(obs_a.credential_epoch().storage_key().to_string());
    let _guard = ports.begin_invocation(&inv).unwrap();

    let denied_cases = [
        (
            "cross-site-cmd",
            CommandAction::SiteMaintenance {
                site_handle: site_b.to_string(),
                action: MaintenanceAction::CheckIn,
            },
        ),
        (
            "global-sched-cmd",
            CommandAction::ScheduleSet {
                task_id: "sync".into(),
                interval_secs: 300,
                enabled: true,
                expected_revision: 0,
            },
        ),
        (
            "global-maint-cmd",
            CommandAction::SiteMaintenance {
                site_handle: "global".into(),
                action: MaintenanceAction::CheckIn,
            },
        ),
    ];
    for (key, bad_action) in denied_cases {
        let err = ports
            .commands_submit(&inv, cmd_req(key, bad_action))
            .unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::PermissionDenied,
            "expected deny on {key}"
        );
    }
    let same_site = CommandAction::SiteMaintenance {
        site_handle: site_a.to_string(),
        action: MaintenanceAction::CheckIn,
    };
    let ok_op_id = ports
        .commands_submit(&inv, cmd_req("same-site-cmd", same_site))
        .unwrap();
    assert!(uuid::Uuid::parse_str(&ok_op_id).is_ok());
}

fn build_cred_bound_ui_zip(plugin_id: &str) -> (Vec<u8>, String) {
    use sha2::{Digest, Sha256};
    let remote_js = b"export const remote = true;\n";
    let js_sha: String = Sha256::digest(remote_js)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{ "path": "ui/assets/remoteEntry.js", "sha256": js_sha, "size": remote_js.len(), "content_type": "application/javascript" }]
    }))
    .unwrap();
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "CredBound UI"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Credential bound UI command test"
config_schema = "schemas/config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["http:site-auth", "command:site.maintenance"]

[[components.tasks]]
id = "sync"
input_schema = "schemas/task.schema.json"
schedule = {{ kind = "interval", seconds = 3600 }}

[ui]
kind = "federation-react"
contract = "1.0.0"
react = "19.3.0"
react_dom = "19.3.0"
remote_name = "credBoundUiRemote"
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
capabilities = ["read:sites", "command:site.maintenance"]

[[ui.surfaces]]
kind = "page"
id = "main"
expose = "./Page"
title = "Main"
"#
    );
    let mut entries = std::collections::BTreeMap::new();
    entries.insert("plugin.toml".into(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".into(),
        pkg::sample_schema(false, None).into_bytes(),
    );
    entries.insert(
        "schemas/task.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#.to_vec(),
    );
    entries.insert("components/worker.wasm".into(), pkg::task_wasm().to_vec());
    entries.insert("ui/asset-manifest.json".into(), asset_manifest);
    entries.insert("ui/assets/remoteEntry.js".into(), remote_js.to_vec());
    let bytes = plugin_package::build_deterministic_cmplugin_zip(&entries).unwrap();
    let pkg_sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (bytes, pkg_sha)
}

#[tokio::test]
async fn p1_ui_command_resolves_handle_targets_for_credential_bound_and_site_scoped_grants() {
    let env = HttpEnv::new().await;
    let key = pkg::TestSigningKey::generate(811);
    env.pin_trust_key(&key, "k-811").await;
    let site_a = env.insert_site("CredBoundSiteA");
    let site_b = env.insert_site("CredBoundSiteB");
    let pid = "org.example.cbui";
    let (zip_bytes, pkg_hash) = build_cred_bound_ui_zip(pid);
    let grants = json!([
        { "capability": "http:site-auth", "scope": { "Site": site_a.to_string() }, "sensitive": false },
        { "capability": "command:site.maintenance", "scope": { "Site": site_a.to_string() }, "sensitive": false },
    ]);
    let inst = env
        .install_pkg(&zip_bytes, Some(&key.sign_package(&zip_bytes)), grants)
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-cbui")
        .await;

    let (_, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep_d = &b_p["data"];
    let reauth = env.reauth("plugin-ui-trust").await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "cbui-tg")],
            Some(json!({
                "preparation_id": prep_d["preparation_id"],
                "package_hash": prep_d["package_hash"],
                "consent_token": prep_d["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK);

    let attr = [
        ("x-plugin-package", pkg_hash.as_str()),
        ("x-plugin-generation", "1"),
    ];
    let resolve_handle = |sid: domain::SiteId| {
        let env = &env;
        async move {
            let (st, _, b) = env
                .send(req_json(
                    "POST",
                    &format!("/api/v1/plugins/{pid}/ui/query"),
                    Some(TOKEN),
                    &attr,
                    Some(json!({ "name": "handles.resolve", "input": { "kind": "site", "id": sid } })),
                ))
                .await;
            assert_ok(st, &b, StatusCode::OK)["handle"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let handle_a = resolve_handle(site_a).await;
    let handle_b = resolve_handle(site_b).await;

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[attr[0], attr[1], ("idempotency-key", "cbui-cmd-ok")],
            Some(json!({ "kind": "site.check-in", "payload": { "site_handle": handle_a } })),
        ))
        .await;
    let op_first = assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);

    let (st_rep, _, b_rep) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[attr[0], attr[1], ("idempotency-key", "cbui-cmd-ok")],
            Some(json!({ "kind": "site.check-in", "payload": { "site_handle": handle_a } })),
        ))
        .await;
    let op_replay = assert_ok(st_rep, &b_rep, StatusCode::ACCEPTED);
    assert_eq!(op_replay["operation_id"], op_first["operation_id"]);

    let (st_deny, _, b_deny) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[attr[0], attr[1], ("idempotency-key", "cbui-cmd-deny")],
            Some(json!({ "kind": "site.check-in", "payload": { "site_handle": handle_b } })),
        ))
        .await;
    assert_eq!(st_deny, StatusCode::FORBIDDEN, "got {b_deny:?}");
}

#[tokio::test]
async fn p1_verify_site_rejects_uninstalled_plugin_profile_without_raw_get() {
    let env = HttpEnv::new().await;
    let sid = domain::SiteId::new();
    env.state
        .store()
        .insert_site(&domain::Site {
            id: sid,
            name: "UninstalledPluginSite".into(),
            url: "http://127.0.0.1:1/".into(),
            profile_id: "plugin:org.example.missing/idx".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{sid}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let data = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(data["ok"], false);
    assert!(
        data["error"]
            .as_str()
            .unwrap_or("")
            .contains("不可用或已停用")
    );
}

fn seed_boost_retained_task(
    store: &store::Store,
    pid: &str,
    dl_id: domain::DownloaderId,
    id: &str,
    hash: &str,
    now: i64,
) {
    use store::download_operations::*;
    let op_id = format!("op-{id}");
    let tag = format!("tag-{id}");
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: op_id.clone(),
            idempotency_key: format!("ik-{id}"),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "seed".into(),
            site_id: None,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: format!("https://pt.example/{id}.torrent"),
            ownership_tag: tag.clone(),
            info_hash: Some(hash.into()),
            bytes_digest: Some(format!("dig-{id}")),
            verified_bytes: Some(512),
            request_digest: format!("req-{id}"),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: tag.clone(),
            info_hash: Some(hash.into()),
            operation_id: op_id,
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: None,
            enclosure: format!("https://pt.example/{id}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(512),
            bytes_digest: Some(format!("dig-{id}")),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: id.into(),
            plugin_id: pid.into(),
            quota_key: format!("{pid}:{dl_id}"),
            downloader_id: dl_id,
            ownership_tag: tag,
            info_hash: hash.into(),
            site_id: None,
            state: BoostTaskState::Seeding,
            occupied_bytes: 512,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap();
    store.mark_boost_task_retained(id, 1, 512, now).unwrap();
}

#[tokio::test]
async fn p2_downloader_scoped_grants_infer_target_from_task_and_plan_handles() {
    use store::download_operations::PrepareBoostCleanupPlanInput;
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let dl_a = env.insert_downloader("DLScopeA");
    let pid = "org.example.dlscope";
    let zip = build_task_zip(
        pid,
        &[
            "command:downloader.prepare-cleanup",
            "command:downloader.delete-files",
        ],
    );
    let scope_a = json!({ "Object": { "kind": "downloader", "id": dl_a.to_string() } });
    let inst = env
        .install_pkg(
            &zip,
            None,
            json!([
                { "capability": "command:downloader.prepare-cleanup", "scope": scope_a, "sensitive": false },
                { "capability": "command:downloader.delete-files", "scope": scope_a, "sensitive": false },
            ]),
        )
        .await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-dlscope")
        .await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let store = env.state.store();
    seed_boost_retained_task(
        &store,
        pid,
        dl_a,
        "bf-1",
        "1111111111111111111111111111111111111111",
        now,
    );
    seed_boost_retained_task(
        &store,
        pid,
        dl_a,
        "bf-2",
        "2222222222222222222222222222222222222222",
        now,
    );
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "bf-2".into(),
            expected_revision: 2,
            cleanup_plan_handle: "plan-h-2".into(),
            cleanup_plan_json: r#"{"files":["a.mkv"]}"#.into(),
            ttl_secs: 600,
            now_utc: now,
        })
        .unwrap();

    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[("idempotency-key", "dlscope-prep")],
            Some(json!({
                "kind": "downloader.prepare-cleanup",
                "payload": { "targets": [{ "task_id": "bf-1", "expected_revision": 2 }] }
            })),
        ))
        .await;
    assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED);

    let (st_del, _, b_del) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[("idempotency-key", "dlscope-del")],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": { "host_file_plan_handle": "plan-h-2", "confirmation_token": "bad-tok" }
            })),
        ))
        .await;
    let err_del = &b_del;
    assert_eq!(
        err_del["error"]["code"], "plugin.cleanup_token_invalid",
        "must pass authorize_plugin_command and ensure_dl_target and reach cleanup_tokens.verify: {err_del}"
    );
    assert_eq!(st_del, StatusCode::FORBIDDEN, "got {err_del}");
}
