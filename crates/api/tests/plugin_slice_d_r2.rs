#[path = "builtin_boost/support.rs"]
mod support;

use std::time::Duration;

use api::plugins::StageUploadInput;
use axum::http::StatusCode;
use domain::{DownloaderId, SiteId};
use hooks::{Gate, HookStepStatus, NotifyErrorDisposition};
use indexer::{MaintenanceAction, MaintenanceCallContext};
use plugin_api::{
    Candidate, CommandAction, CommandRequest, DownloadBefore, DownloadPlanView, ErrorCode, Event,
    EventPayload, GuestRole, HOOK_WORLD, HookContext, PageRequest, SiteCheckinAfterEvent,
};
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::Store;
use store::plugin_events::{EmitOutboxEventInput, OutboxDb};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginGrant,
    PluginGrantScope, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};
use uuid::Uuid;

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

fn canon_req(method: &str, path: &str, tag: u8) -> CanonicalPluginRequest {
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: [tag; 32],
    }
}

fn grant(cap: &str, scope: PluginGrantScope) -> PluginGrant {
    PluginGrant {
        capability: cap.into(),
        scope,
        sensitive: false,
    }
}

fn hook_ctx(site_id: Option<SiteId>) -> HookContext {
    HookContext {
        invocation_id: format!("inv-{}", Uuid::new_v4().simple()),
        cause_id: "cause-r2".into(),
        plugin_id: String::new(),
        generation: 1,
        config_revision: 1,
        subject: "system".into(),
        site_id: site_id.map(|s| s.to_string()),
        subscribe_id: None,
        library_id: None,
        deadline_ms: 500,
        cancelled: false,
    }
}

fn dl_before_input(site_id: Option<SiteId>, dl_id: DownloaderId) -> plugin_api::GateInput {
    let candidate = Candidate {
        handle: "h1".into(),
        site_id: site_id.map_or_else(String::new, |s| s.to_string()),
        provider: "demo".into(),
        generation: 1,
        title: "Test.1080p".into(),
        size: Some(1024),
        seeders: Some(5),
        leechers: Some(1),
        published_at: None,
        free: Some(true),
        hr: Some(false),
        release: None,
        score: None,
        labels: vec![],
        revision: 1,
    };
    let plan = DownloadPlanView {
        downloader_id: dl_id.to_string(),
        relative_directory: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    plugin_api::GateInput {
        schema_version: 1,
        context: hook_ctx(site_id),
        hook_id: "download.before".into(),
        handles: vec!["h1".into()],
        revision: 1,
        payload: plugin_api::GatePayload::DownloadBefore(Box::new(DownloadBefore {
            candidate,
            purpose: "subscribe".into(),
            plan,
        })),
    }
}

fn build_policy_zip(id: &str, cred_bound: bool, hooks_toml: &str, perms: &[&str]) -> Vec<u8> {
    let perms_str = serde_json::to_string(perms).unwrap();
    let cred = if cred_bound {
        "credential_binding = \"invocation-site\"\n"
    } else {
        ""
    };
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        "manifest_version=1\nid=\"{id}\"\nname=\"P\"\nversion=\"1.0.0\"\napi=\"1.0.0\"\nlicense=\"MIT\"\nauthors=[\"t\"]\ndescription=\"d\"\nconfig_schema=\"config.schema.json\"\nstate_schema=1\n{cred}[[components]]\nid=\"policy\"\nentry=\"policy.wasm\"\nworld=\"{HOOK_WORLD}\"\npermissions={perms_str}\n\n{hooks_toml}\n"
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ])
}

fn build_site_cfg_task_zip(id: &str, version: &str) -> (Vec<u8>, String) {
    let schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"site_id":{"type":"string"},"downloader_id":{"type":"string"}}}"#;
    let raw: Value = serde_json::from_str(schema).unwrap();
    let rev = plugin_package::validate_config_schema(&raw)
        .unwrap()
        .schema_revision()
        .to_string();
    let manifest = pkg::task_manifest_with_options(id, version, 1, false, &["kv"]);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", pkg::task_wasm()),
    ]);
    (zip, rev)
}

async fn install_and_enable(env: &HttpEnv, pid: &str, zip: &[u8], grants: Value) {
    let rev = env.install_pkg(zip, None, grants).await["revision"]
        .as_i64()
        .unwrap();
    env.enable_plugin(pid, rev, &format!("en-{}", Uuid::new_v4().simple()))
        .await;
}

async fn post_prep_ok(env: &HttpEnv, path: &str, body: Value) -> Value {
    let (st, _, b) = env
        .send(req_json("POST", path, Some(TOKEN), &[], Some(body)))
        .await;
    assert_ok(st, &b, StatusCode::OK)
}

async fn prep_up_http(
    env: &HttpEnv,
    t: (&str, &str, &str),
    p: (&str, i64, u8, Value),
) -> (StatusCode, Value) {
    let ((pid, uid, srev), (policy, rev, tag, vals)) = (t, p);
    let (rev_s, idem) = (format!("\"{rev}\""), format!("pu-{tag}"));
    let body = json!({ "version": "1.1.0", "upload_id": uid, "state_policy": policy, "target_config": { "schema_revision": srev, "values": vals } });
    let req = req_json(
        "POST",
        &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        Some(TOKEN),
        &[("if-match", &rev_s), ("idempotency-key", &idem)],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

async fn exec_up_http(
    env: &HttpEnv,
    pid: &str,
    prep: &Value,
    p: (&str, i64, u8),
) -> (StatusCode, Value) {
    let (policy, rev, tag) = p;
    let (rev_s, idem) = (format!("\"{rev}\""), format!("eu-{tag}"));
    let body = json!({ "preparation_id": prep["preparation_id"], "consent_token": prep["consent_token"], "grants": [], "state_policy": policy, "target_config_digest": prep["target_config_digest"] });
    let req = req_json(
        "POST",
        &format!("/api/v1/plugins/{pid}/upgrade"),
        Some(TOKEN),
        &[("if-match", &rev_s), ("idempotency-key", &idem)],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

async fn install_v1(env: &HttpEnv, pid: &str) -> i64 {
    let zip = pkg::build_task_package(pid, "1.0.0", 1, false);
    env.install_pkg(&zip, None, json!([])).await["revision"]
        .as_i64()
        .unwrap()
}

async fn build_item1_specs(env: &HttpEnv) -> (Vec<u8>, String, Vec<(String, i64, Value)>) {
    let (pi, pg, ps, pu) = (
        "org.example.r2i",
        "org.example.r2g",
        "org.example.r2s",
        "org.example.r2u",
    );
    let zip_i = pkg::build_task_package(pi, "1.0.0", 1, false);
    let uid_i = env.upload_pkg(&zip_i, None, "up-i").await["upload_id"]
        .as_str()
        .unwrap()
        .to_string();
    let req_pi = req_json(
        "POST",
        "/api/v1/plugin-installations/prepare",
        Some(TOKEN),
        &[("if-match", "\"0\""), ("idempotency-key", "pi-1")],
        Some(json!({ "upload_id": uid_i })),
    );
    let (st_pi, _, b_pi) = env.send(req_pi).await;
    let prep_i = &assert_ok(st_pi, &b_pi, StatusCode::ACCEPTED)["result"];

    let rev_g = install_v1(env, pg).await;
    let grants_g = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let prep_g = post_prep_ok(
        env,
        &format!("/api/v1/plugins/{pg}/permissions/prepare"),
        json!({ "requested_grants": grants_g }),
    )
    .await;

    let rev_s = install_v1(env, ps).await;
    let prep_s = post_prep_ok(
        env,
        &format!("/api/v1/plugins/{ps}/state/reset/prepare"),
        json!({}),
    )
    .await;
    let reauth_s = env.reauth("plugin:state-reset").await;

    let rev_u = install_v1(env, pu).await;
    let zip_u2 = pkg::build_task_package(pu, "1.1.0", 2, false);
    let uid_u2 = env.upload_pkg(&zip_u2, None, "up-u2").await["upload_id"]
        .as_str()
        .unwrap()
        .to_string();
    let srev = pkg::schema_rev(false, None);
    let (st_pu, b_pu) = prep_up_http(
        env,
        (pu, &uid_u2, &srev),
        ("reset", rev_u, 50, json!({"mode": "ok"})),
    )
    .await;
    let prep_u = &assert_ok(st_pu, &b_pu, StatusCode::ACCEPTED)["result"];
    let reauth_u = env.reauth("plugin:upgrade").await;

    let key = pkg::TestSigningKey::generate(901);
    let pinned = env.pin_trust_key(&key, "r2-k").await;
    let (kid, krev) = (
        pinned["key_id"].as_str().unwrap(),
        pinned["revision"].as_i64().unwrap(),
    );
    let reauth_k = env.reauth("trust-key:revoke").await;
    let reauth_up = env.reauth("plugin:upload").await;

    let specs = vec![
        (
            "/api/v1/plugin-installations".into(),
            0,
            json!({ "preparation_id": prep_i["preparation_id"], "consent_token": prep_i["consent_token"], "grants": [] }),
        ),
        (
            format!("/api/v1/plugins/{pg}/permissions/grant"),
            rev_g,
            json!({ "consent_token": prep_g["consent_token"], "grants": grants_g }),
        ),
        (
            format!("/api/v1/plugins/{ps}/state/reset"),
            rev_s,
            json!({ "security_scopes": [], "consent_token": prep_s["consent_token"], "reauth_token": reauth_s }),
        ),
        (
            format!("/api/v1/plugins/{pu}/upgrade"),
            rev_u,
            json!({ "preparation_id": prep_u["preparation_id"], "consent_token": prep_u["consent_token"], "grants": [], "state_policy": "reset", "target_config_digest": prep_u["target_config_digest"], "reauth_token": reauth_u }),
        ),
        (
            format!("/api/v1/plugin-trust-keys/{kid}/revoke"),
            krev,
            json!({ "reason": "rotate", "reauth_token": reauth_k }),
        ),
    ];
    (zip_i, reauth_up, specs)
}

#[tokio::test]
async fn slice_d_r2_item1_tokens_not_burned_before_app_write_commit() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (zip_i, reauth_up, specs) = build_item1_specs(&env).await;
    let mk_up = |tok: String, tag: u8| StageUploadInput {
        actor_user_id: admin_id(),
        upload_id: Uuid::new_v4(),
        cmplugin_bytes: zip_i.clone(),
        signature_text: None,
        expected_sha256: None,
        reauth_token: tok,
        idempotency_key: format!("up-busy-{tag}"),
        request: canon_req("POST", "/plugin-uploads", tag),
    };
    let post = |(p, r, b): &(String, i64, Value), idem: String| {
        req_json(
            "POST",
            p,
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{r}\"")),
                ("idempotency-key", &idem),
            ],
            Some(b.clone()),
        )
    };

    let store_bg = env.state.store();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        store_bg
            .app_write(|_| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();
    let [f0, f1, f2, f3, f4] =
        std::array::from_fn(|i| env.send(post(&specs[i], format!("b1-{i}"))));
    let (r_up, o0, o1, o2, o3, o4) = tokio::join!(
        env.state
            .plugins()
            .stage_upload(&env.state, mk_up(reauth_up.clone(), 1)),
        f0,
        f1,
        f2,
        f3,
        f4,
    );
    let _ = release_tx.send(());
    holder.join().unwrap();
    assert_eq!(r_up.unwrap_err().status_and_code(), (503, "runtime.busy"));
    for (st, _, b) in [o0, o1, o2, o3, o4] {
        assert_err(st, &b, StatusCode::SERVICE_UNAVAILABLE, "runtime.busy");
    }

    assert!(
        !env.state
            .plugins()
            .stage_upload(&env.state, mk_up(reauth_up, 2))
            .await
            .unwrap()
            .hash
            .is_empty()
    );
    for (i, spec) in specs.iter().enumerate() {
        let (st, _, b) = env.send(post(spec, format!("b2-{i}"))).await;
        assert!(
            matches!(st, StatusCode::OK | StatusCode::ACCEPTED),
            "body={b}"
        );
    }
}

fn set_lifecycle(store: &Store, pid: &str, rev: i64, enable: bool) -> store::plugins::PluginRecord {
    let t = store
        .begin_plugin_lifecycle(pid, rev, Uuid::new_v4(), enable)
        .unwrap();
    store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: pid.into(),
            operation_id: t.active_operation_id.unwrap(),
            revision: t.revision,
            generation: t.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

fn replace_grants_and_enable(store: &Store, pid: &str, grants: &[PluginGrant]) {
    let mut p = store.get_plugin(pid).unwrap().unwrap();
    if p.lifecycle == store::plugins::PluginLifecycle::Enabled {
        p = set_lifecycle(store, pid, p.revision, false);
    }
    let g = store
        .replace_plugin_grants(pid, &p.package_hash, p.revision, grants)
        .unwrap();
    set_lifecycle(store, pid, g.revision, true);
}

fn assert_nexusphp_and_boost_scopes(env: &HttpEnv, site_a: SiteId, site_b: SiteId) -> String {
    let store = env.state.store();
    let (nx_id, nx_prov) = (
        "builtin.nexusphp-maintenance",
        "plugin:builtin.nexusphp-maintenance/default",
    );
    replace_grants_and_enable(
        &store,
        nx_id,
        &[grant("read:sites", PluginGrantScope::Global)],
    );
    api::plugins::sync_registry_snapshot(&env.state, true).unwrap();
    assert!(
        env.state
            .indexer()
            .snapshot()
            .maintenance(nx_prov)
            .is_none()
    );

    let norm_id = format!("site:{}", site_a.to_string().to_ascii_uppercase());
    let obj_a = PluginGrantScope::Object {
        kind: "site".into(),
        id: norm_id.clone(),
    };
    replace_grants_and_enable(&store, nx_id, &[grant("maintenance:provide", obj_a)]);
    api::plugins::sync_registry_snapshot(&env.state, true).unwrap();
    let snap = env.state.indexer().snapshot();
    let prov = snap.maintenance(nx_prov).unwrap();
    let sb = store.get_site(site_b).unwrap().unwrap();
    let mctx = MaintenanceCallContext {
        fetcher: env.state.indexer().as_ref(),
        generation: 1,
    };
    let err_b = prov
        .provider()
        .run(&mctx, &sb, MaintenanceAction::VerifySession, "job-1")
        .unwrap_err();
    assert!(err_b.to_string().contains("maintenance:provide"));

    let dl_id = DownloaderId::new();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "dl".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    support::save_boost_config(
        &env.state,
        json!({ "downloader_id": dl_id.to_string(), "budget_bytes": 0, "enforce_budget_on_all": true }),
    );
    replace_grants_and_enable(
        &store,
        "builtin.boost",
        &[grant(
            "hooks:download.before",
            PluginGrantScope::Site(site_a),
        )],
    );
    let pipe = api::build_hook_pipeline_for_test(&env.state, "download.before");
    let (ga, _) = pipe
        .gate_typed_with_trace("wf-a", &dl_before_input(Some(site_a), dl_id))
        .unwrap();
    let (gb, _) = pipe
        .gate_typed_with_trace("wf-b", &dl_before_input(Some(site_b), dl_id))
        .unwrap();
    assert!(matches!(ga, Gate::Reject(_)) && matches!(gb, Gate::Allow | Gate::Abstain));
    norm_id
}

#[tokio::test]
async fn slice_d_r2_item2_provider_hook_and_port_grant_scopes() {
    let env = HttpEnv::new().await;
    let (site_a, site_b) = (env.insert_site("SiteA"), env.insert_site("SiteB"));
    let norm_id = assert_nexusphp_and_boost_scopes(&env, site_a, site_b);

    env.enable_dev_mode().await;
    let p_zip = pkg::build_task_package_custom(
        "org.example.portscope",
        "1.0.0",
        1,
        false,
        None,
        false,
        &[
            "read:sites",
            "command:site.maintenance",
            "command:downloader.add",
        ],
    );
    install_and_enable(
        &env,
        "org.example.portscope",
        &p_zip,
        json!([
            { "capability": "read:sites", "scope": { "Object": { "kind": "site", "id": norm_id } }, "sensitive": false },
            { "capability": "command:site.maintenance", "scope": { "Object": { "kind": "site", "id": norm_id } }, "sensitive": false },
            { "capability": "command:downloader.add", "scope": { "Object": { "kind": "site", "id": site_a.to_string() } }, "sensitive": false }
        ]),
    )
    .await;
    let enabled = env
        .state
        .store()
        .get_plugin("org.example.portscope")
        .unwrap()
        .unwrap();
    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new("org.example.portscope", Duration::from_secs(5));
    inv.generation = u64::try_from(enabled.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();

    assert!(ports.site_get(&inv, &site_a.to_string()).is_ok());
    let req_pg = PageRequest {
        limit: 10,
        cursor: None,
    };
    assert_eq!(ports.site_list(&inv, req_pg).unwrap().items.len(), 1);
    assert!(
        ports.authorized(&inv, "command:site.maintenance")
            && !ports.authorized(&inv, "command:downloader.add")
    );
    let action = CommandAction::SiteMaintenance {
        site_handle: format!("site:{site_b}"),
        action: plugin_api::MaintenanceAction::CheckIn,
    };
    let err_t = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action,
                idempotency_key: "m-b".into(),
                expected_revision: None,
            },
        )
        .unwrap_err();
    assert_eq!(err_t.code, ErrorCode::PermissionDenied);
    assert!(err_t.message.contains("command target"));
}

fn checkin_after_ev(sid: SiteId) -> SiteCheckinAfterEvent {
    SiteCheckinAfterEvent {
        site_id: sid.to_string(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-24".into(),
    }
}

async fn assert_notify_dual_grant(env: &HttpEnv, site_a: SiteId, site_b: SiteId) {
    let zip_n = build_policy_zip(
        "org.example.notify-dual",
        false,
        "[[components.hooks]]\nid = \"site.checkin.after\"\npriority = 10\n",
        &["hooks:site.checkin.after", "events:site.checkin.after"],
    );
    let grants_n = json!([
        { "capability": "hooks:site.checkin.after", "scope": "Global", "sensitive": false },
        { "capability": "events:site.checkin.after", "scope": { "Site": site_a.to_string() }, "sensitive": false }
    ]);
    install_and_enable(env, "org.example.notify-dual", &zip_n, grants_n).await;

    let pipe_n = api::build_hook_pipeline_for_test(&env.state, "site.checkin.after");
    let ev_b = Event {
        schema_version: 1,
        context: hook_ctx(Some(site_b)),
        id: "ev-b".into(),
        kind: "site.checkin.after".into(),
        occurred_at: 1_700_000_000,
        handles: vec![],
        revision: 1,
        payload: EventPayload::SiteCheckinAfter(checkin_after_ev(site_b)),
    };
    let (trace_b, outcomes_b) = pipe_n.notify_typed_outcomes_with_trace("wf-nb", &ev_b);
    assert_eq!(trace_b.steps[0].status, HookStepStatus::Error);
    assert_eq!(
        outcomes_b[0].disposition,
        Some(NotifyErrorDisposition::Permanent)
    );
}

#[tokio::test]
async fn slice_d_r2_item3_outbox_notify_deadletter_and_disabled_site_gate() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (site_a, site_b, site_dis) = (
        env.insert_site("A"),
        env.insert_site("B"),
        env.insert_site("Dis"),
    );
    env.state
        .store()
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE sites SET enabled = 0, cookie = 'uid=1' WHERE id = ?1",
                [site_dis.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_notify_dual_grant(&env, site_a, site_b).await;

    let hooks_cb = "[[components.hooks]]\nid=\"site.checkin.after\"\npriority=10\n[[components.hooks]]\nid=\"download.before\"\npriority=10\n";
    let caps_cb = [
        "http:site-auth",
        "hooks:site.checkin.after",
        "events:site.checkin.after",
        "hooks:download.before",
    ];
    let zip_cb = build_policy_zip("org.example.cb-dis", true, hooks_cb, &caps_cb);
    let grants_cb = json!(caps_cb.map(
        |c| json!({ "capability": c, "scope": "Global", "sensitive": c == "http:site-auth" })
    ));
    install_and_enable(&env, "org.example.cb-dis", &zip_cb, grants_cb).await;

    let now_ms = store::unix_now_ms().unwrap();
    let ev_json = serde_json::to_string(&checkin_after_ev(site_dis)).unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: "ev-dis-site".into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: site_dis.to_string(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "c-dis".into(),
                subject: "system".into(),
                site_id: Some(site_dis.to_string()),
                subscribe_id: None,
                library_id: None,
                handles_json: "[]".into(),
                payload_json: ev_json,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
    api::drain_plugin_events_for_test(&env.state, now_ms).await;
    assert!(
        env.state
            .store()
            .get_delivery_receipt(OutboxDb::App, "ev-dis-site", "org.example.cb-dis")
            .unwrap()
            .is_none()
    );

    let pipe_dl = api::build_hook_pipeline_for_test(&env.state, "download.before");
    let (gate_dis, _) = pipe_dl
        .gate_typed_with_trace(
            "wf-dis",
            &dl_before_input(Some(site_dis), DownloaderId::new()),
        )
        .unwrap();
    assert!(matches!(gate_dis, Gate::Allow | Gate::Abstain));
}

#[tokio::test]
async fn slice_d_r2_item4_upgrade_validate_before_register_and_failed_gc() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.upgc";
    let rev1 = install_v1(&env, pid).await;
    let zip2 =
        pkg::build_task_package_custom(pid, "1.1.0", 1, false, Some("reject"), false, &["kv"]);
    let up2 = env.upload_pkg(&zip2, None, "up-gc-v2").await;
    let (v2_hash, u2) = (
        up2["hash"].as_str().unwrap().to_string(),
        up2["upload_id"].as_str().unwrap().to_string(),
    );
    let srev_rej = pkg::schema_rev(false, Some("reject"));

    let (st_p, bp) = prep_up_http(
        &env,
        (pid, &u2, &srev_rej),
        ("preserve", rev1, 71, json!({"mode": "ok"})),
    )
    .await;
    let prep = &assert_ok(st_p, &bp, StatusCode::ACCEPTED)["result"];
    let (st_e, be) = exec_up_http(&env, pid, prep, ("preserve", rev1, 72)).await;
    assert_err(
        st_e,
        &be,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_component",
    );
    assert!(store.get_plugin_version(pid, &v2_hash).unwrap().is_none());
    assert!(
        !store
            .data_dir()
            .join("plugins/packages")
            .join(&v2_hash)
            .exists()
    );

    let cur_rev = env.get_plugin_rev(pid).await;
    let digest2 = prep["target_config_digest"].as_str().unwrap().to_string();
    store
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "sim-crash-up".into(),
                request: canon_req("POST", "/upgrade", 74),
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur_rev,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(v2_hash.clone()),
                    version: Some("1.1.0".into()),
                    artifact_path: Some(format!("plugins/uploads/{u2}/package.cmplugin")),
                    state_policy: Some("preserve".into()),
                    target_config_digest: Some(digest2),
                    grants: vec![],
                },
                cause: PluginCommandCause::root("crash-test"),
            })?;
            let _ = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )?;
            Ok(())
        })
        .unwrap();
    api::recover_plugin_startup_for_test(&env.state).unwrap();
    assert!(store.get_plugin_version(pid, &v2_hash).unwrap().is_none());
}

#[tokio::test]
async fn slice_d_r2_item5_upgrade_validates_config_object_grants() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.upcfg";
    let (zip1, sr) = build_site_cfg_task_zip(pid, "1.0.0");
    let (zip2, _) = build_site_cfg_task_zip(pid, "1.1.0");
    let rev1 = env.install_pkg(&zip1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let u2 = env.upload_pkg(&zip2, None, "up-cfg-v2").await["upload_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st1, b1) = prep_up_http(
        &env,
        (pid, &u2, &sr),
        ("preserve", rev1, 81, json!({ "site_id": SiteId::new() })),
    )
    .await;
    assert_err(st1, &b1, StatusCode::BAD_REQUEST, "plugin.scope_invalid");

    let site_del = env.insert_site("TempSite");
    let p_after = store
        .save_plugin_config(pid, rev1, &sr, &json!({ "site_id": site_del }))
        .unwrap();
    store.delete_site(site_del).unwrap();

    let (st2, b2) = prep_up_http(
        &env,
        (pid, &u2, &sr),
        ("preserve", p_after.revision, 82, json!({})),
    )
    .await;
    assert_err(st2, &b2, StatusCode::BAD_REQUEST, "plugin.scope_invalid");

    let site_race = env.insert_site("RaceSite");
    let rv = json!({ "site_id": site_race });
    let p_ok = store
        .save_plugin_config(pid, p_after.revision, &sr, &rv)
        .unwrap();
    let (st_ok, b_ok) =
        prep_up_http(&env, (pid, &u2, &sr), ("preserve", p_ok.revision, 83, rv)).await;
    let prep_ok = &assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED)["result"];
    store.delete_site(site_race).unwrap();

    let (st3, b3) = exec_up_http(&env, pid, prep_ok, ("preserve", p_ok.revision, 84)).await;
    assert_err(st3, &b3, StatusCode::BAD_REQUEST, "plugin.scope_invalid");
}
