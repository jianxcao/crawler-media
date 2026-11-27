#[path = "builtin_boost/support.rs"]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::SiteId;
use net_guard::TargetPolicy;
use plugin_api::{ErrorCode, GuestRole, HttpRequest, HttpTarget, SiteTarget};
use plugin_host::Invocation;
use plugin_package::validate_cmplugin_bytes;
use serde_json::{Value, json};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginGrant,
    PluginGrantScope, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};
use uuid::Uuid;

fn canon_req(method: &str, path: &str, tag: u8) -> CanonicalPluginRequest {
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: [tag; 32],
    }
}

fn req_dev_raw(bytes: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/plugins/dev")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(bytes))
        .unwrap()
}

async fn post_write(
    env: &HttpEnv,
    path: impl AsRef<str>,
    rev: i64,
    idem: impl AsRef<str>,
    body: Value,
) -> (StatusCode, Value) {
    let r = format!("\"{rev}\"");
    let req = req_json(
        "POST",
        path.as_ref(),
        Some(TOKEN),
        &[("if-match", &r), ("idempotency-key", idem.as_ref())],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

async fn post_prep(env: &HttpEnv, path: impl AsRef<str>, body: Value) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json(
            "POST",
            path.as_ref(),
            Some(TOKEN),
            &[],
            Some(body),
        ))
        .await;
    (st, b)
}

fn obj_site_grant(cap: &str, id: impl Into<String>) -> Value {
    json!([{ "capability": cap, "scope": { "Object": { "kind": "site", "id": id.into() } }, "sensitive": false }])
}

fn build_site_cfg_task_zip(id: &str, version: &str, perms: &[&str]) -> (Vec<u8>, String) {
    let schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"site_id":{"type":"string"}}}"#;
    let raw: Value = serde_json::from_str(schema).unwrap();
    let rev = plugin_package::validate_config_schema(&raw)
        .unwrap()
        .schema_revision()
        .to_string();
    let manifest = pkg::task_manifest_with_options(id, version, 1, false, perms);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", pkg::task_wasm()),
    ]);
    (zip, rev)
}

fn sha256_hex_of_pkg(bytes: &[u8]) -> String {
    let policy = plugin_package::PackagePolicy {
        dev_mode: true,
        ..Default::default()
    };
    validate_cmplugin_bytes(bytes, &policy)
        .unwrap()
        .package_sha256_hex
}

async fn prep_install_http(env: &HttpEnv, zip: &[u8], tag: &str) -> Value {
    let up = env.upload_pkg(zip, None, &format!("up-{tag}")).await;
    let uid = up["upload_id"].as_str().unwrap();
    let (st, b) = post_write(
        env,
        "/api/v1/plugin-installations/prepare",
        0,
        format!("pi-{tag}"),
        json!({ "upload_id": uid }),
    )
    .await;
    assert_ok(st, &b, StatusCode::ACCEPTED)["result"].clone()
}

async fn prep_upgrade_http(
    env: &HttpEnv,
    t: (&str, &str, &str),
    rev: i64,
    vals: Value,
    tag: &str,
) -> Value {
    let (pid, uid, srev) = t;
    let body = json!({ "version": "1.1.0", "upload_id": uid, "state_policy": "preserve", "target_config": { "schema_revision": srev, "values": vals } });
    let (st, b) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/upgrades/prepare"),
        rev,
        format!("pu-{tag}"),
        body,
    )
    .await;
    assert_ok(st, &b, StatusCode::ACCEPTED)["result"].clone()
}

fn task_pkg(id: &str, version: &str, perms: &[&str]) -> Vec<u8> {
    pkg::build_task_package_custom(id, version, 1, false, None, false, perms)
}

async fn assert_bad_scope_write(
    env: &HttpEnv,
    path: impl AsRef<str>,
    rev: i64,
    idem: &str,
    body: Value,
) {
    let (st, b) = post_write(env, path, rev, idem, body).await;
    assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.scope_invalid");
}

async fn assert_bad_scope_race(
    env: &HttpEnv,
    site_id: SiteId,
    path: String,
    rev: i64,
    idem: &str,
    body: Value,
) {
    let store = env.state.store();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        store
            .app_write(|app| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
                app.delete_site(site_id)?;
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();
    let ((st, b), _) = tokio::join!(post_write(env, path, rev, idem, body), async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let _ = release_tx.send(());
        handle.join().unwrap();
    });
    assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.scope_invalid");
}

#[tokio::test]
async fn slice_d_r3_item1_zero_capability_manifest_enforces_grant_bounds() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid_zero = "org.example.r3-zerocap";
    env.install_pkg(&task_pkg(pid_zero, "1.0.0", &[]), None, json!([]))
        .await;
    let zero_prep = format!("/api/v1/plugins/{pid_zero}/permissions/prepare");
    let (st_ok, b_ok) = post_prep(&env, &zero_prep, json!({ "requested_grants": [] })).await;
    assert_ok(st_ok, &b_ok, StatusCode::OK);

    let kv_grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let (st_bad, b_bad) =
        post_prep(&env, &zero_prep, json!({ "requested_grants": kv_grants })).await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.scope_invalid",
    );

    let pid_tx = "org.example.r3-granttx";
    let rev_tx = env
        .install_pkg(&task_pkg(pid_tx, "1.0.0", &["kv"]), None, json!([]))
        .await["revision"]
        .as_i64()
        .unwrap();
    let tx_prep = format!("/api/v1/plugins/{pid_tx}/permissions/prepare");
    let (st_p, b_p) = post_prep(&env, &tx_prep, json!({ "requested_grants": kv_grants })).await;
    let prep_tx = assert_ok(st_p, &b_p, StatusCode::OK);

    env.state
        .store()
        .with_writer(store::Db::App, |conn| {
            conn.execute(
                "UPDATE plugin_versions SET manifest_json = (SELECT manifest_json FROM plugin_versions WHERE plugin_id = ?1) WHERE plugin_id = ?2",
                rusqlite::params![pid_zero, pid_tx],
            )?;
            Ok(())
        })
        .unwrap();

    assert_bad_scope_write(
        &env,
        format!("/api/v1/plugins/{pid_tx}/permissions/grant"),
        rev_tx,
        "r3-grant-tx-1",
        json!({ "consent_token": prep_tx["consent_token"], "grants": kv_grants }),
    )
    .await;
}

#[tokio::test]
async fn slice_d_r3_item2_object_site_scope_and_app_write_revalidation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid_perm = "org.example.r3-perm-scope";
    let zip_perm = task_pkg(pid_perm, "1.0.0", &["read:sites"]);
    let rev_perm = env.install_pkg(&zip_perm, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let perm_prep = format!("/api/v1/plugins/{pid_perm}/permissions/prepare");

    for bad_id in ["not-a-uuid".to_string(), format!("site:{}", SiteId::new())] {
        let (st, b) = post_prep(
            &env,
            &perm_prep,
            json!({ "requested_grants": obj_site_grant("read:sites", bad_id) }),
        )
        .await;
        assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.scope_invalid");
    }

    let zip_inst = task_pkg("org.example.r3-inst-scope", "1.0.0", &["read:sites"]);
    let prep_i = prep_install_http(&env, &zip_inst, "r3-inst").await;
    let missing_obj = obj_site_grant("read:sites", format!("site:{}", SiteId::new()));
    assert_bad_scope_write(
        &env,
        "/api/v1/plugin-installations",
        0,
        "r3-i-bad",
        json!({ "preparation_id": prep_i["preparation_id"], "consent_token": prep_i["consent_token"], "grants": missing_obj }),
    )
    .await;

    let site_perm = env.insert_site("RacePerm");
    let perm_grants = obj_site_grant("read:sites", format!("site:{site_perm}"));
    let (st_pp, b_pp) =
        post_prep(&env, &perm_prep, json!({ "requested_grants": perm_grants })).await;
    let prep_p = assert_ok(st_pp, &b_pp, StatusCode::OK);
    assert_bad_scope_race(
        &env,
        site_perm,
        format!("/api/v1/plugins/{pid_perm}/permissions/grant"),
        rev_perm,
        "r3-p-race",
        json!({ "consent_token": prep_p["consent_token"], "grants": perm_grants }),
    )
    .await;

    assert_install_and_upgrade_site_deletion_races(&env, &prep_i).await;
}

async fn assert_install_and_upgrade_site_deletion_races(env: &HttpEnv, prep_i: &Value) {
    let site_inst = env.insert_site("RaceInst");
    let inst_grants =
        json!([{ "capability": "read:sites", "scope": { "Site": site_inst }, "sensitive": false }]);
    assert_bad_scope_race(
        env,
        site_inst,
        "/api/v1/plugin-installations".into(),
        0,
        "r3-i-race",
        json!({ "preparation_id": prep_i["preparation_id"], "consent_token": prep_i["consent_token"], "grants": inst_grants }),
    )
    .await;

    let pid_up = "org.example.r3-up-scope";
    let z1 = task_pkg(pid_up, "1.0.0", &["read:sites"]);
    let z2 = task_pkg(pid_up, "1.1.0", &["read:sites"]);
    let rev_u = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let uid_u2 = env.upload_pkg(&z2, None, "up-r3-u2").await["upload_id"]
        .as_str()
        .unwrap()
        .to_string();
    let prep_u = prep_upgrade_http(
        env,
        (pid_up, &uid_u2, &pkg::schema_rev(false, None)),
        rev_u,
        json!({"mode": "ok"}),
        "r3-u2",
    )
    .await;

    let site_sw = env.insert_site("RaceSwitch");
    let sw_grants = obj_site_grant("read:sites", format!("site:{site_sw}"));
    assert_bad_scope_race(
        env,
        site_sw,
        format!("/api/v1/plugins/{pid_up}/upgrade"),
        rev_u,
        "r3-sw-race",
        json!({
            "preparation_id": prep_u["preparation_id"],
            "consent_token": prep_u["consent_token"],
            "grants": sw_grants,
            "state_policy": "preserve",
            "target_config_digest": prep_u["target_config_digest"],
        }),
    )
    .await;
}

fn seed_claimed_staged_op(
    store: &store::Store,
    kind: StagedPluginOpKind,
    expected_rev: i64,
    intent: StagedPluginIntent,
    tag: u8,
) -> (Uuid, i64) {
    store
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("r3-rec-{tag}"),
                request: canon_req("POST", "/rec", tag),
                op_kind: kind,
                expected_revision: expected_rev,
                intent,
                cause: PluginCommandCause::root(format!("r3-rec:{tag}")),
            })?;
            let c = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )?;
            Ok((c.operation_id(), c.revision()))
        })
        .unwrap()
}

fn upg_intent(pid: &str, up: &Value, prep: &Value, grants: Vec<PluginGrant>) -> StagedPluginIntent {
    StagedPluginIntent {
        target_plugin_id: pid.into(),
        package_hash: Some(up["hash"].as_str().unwrap().into()),
        version: Some("1.1.0".into()),
        artifact_path: Some(format!(
            "plugins/uploads/{}/package.cmplugin",
            up["upload_id"].as_str().unwrap()
        )),
        state_policy: Some("preserve".into()),
        target_config_digest: Some(prep["target_config_digest"].as_str().unwrap().into()),
        grants,
    }
}

fn inst_intent(pid: &str, up: &Value, grants: Vec<PluginGrant>) -> StagedPluginIntent {
    StagedPluginIntent {
        target_plugin_id: pid.into(),
        package_hash: Some(up["hash"].as_str().unwrap().into()),
        version: Some("1.0.0".into()),
        artifact_path: Some(format!(
            "plugins/uploads/{}/package.cmplugin",
            up["upload_id"].as_str().unwrap()
        )),
        state_policy: None,
        target_config_digest: None,
        grants,
    }
}

#[tokio::test]
async fn slice_d_r3_item3_recovery_validates_scopes_and_gcs_failed_resume_artifacts() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid_cfg = "org.example.r3-rec-cfg";
    let (z1, sr) = build_site_cfg_task_zip(pid_cfg, "1.0.0", &["kv"]);
    let (z2, _) = build_site_cfg_task_zip(pid_cfg, "1.1.0", &["kv"]);
    let rev1 = env.install_pkg(&z1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let site_c = env.insert_site("RecCfgSite");
    let saved = store
        .save_plugin_config(pid_cfg, rev1, &sr, &json!({ "site_id": site_c }))
        .unwrap();
    let up_c2 = env.upload_pkg(&z2, None, "up-rec-c2").await;
    let u_c2 = up_c2["upload_id"].as_str().unwrap();
    let prep_c = prep_upgrade_http(
        &env,
        (pid_cfg, u_c2, &sr),
        saved.revision,
        json!({ "site_id": site_c }),
        "rec-c",
    )
    .await;
    store.delete_site(site_c).unwrap();
    seed_claimed_staged_op(
        &store,
        StagedPluginOpKind::Upgrade,
        saved.revision,
        upg_intent(pid_cfg, &up_c2, &prep_c, vec![]),
        31,
    );
    api::recover_plugin_startup_for_test(&env.state).unwrap();
    assert!(
        store
            .get_plugin_version(pid_cfg, up_c2["hash"].as_str().unwrap())
            .unwrap()
            .is_none()
    );

    let pid_ug = "org.example.r3-rec-ug";
    let zu1 = task_pkg(pid_ug, "1.0.0", &["read:sites"]);
    let zu2 = task_pkg(pid_ug, "1.1.0", &["read:sites"]);
    let rev_ug = env.install_pkg(&zu1, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let up_ug2 = env.upload_pkg(&zu2, None, "up-rec-ug2").await;
    let prep_ug = prep_upgrade_http(
        &env,
        (
            pid_ug,
            up_ug2["upload_id"].as_str().unwrap(),
            &pkg::schema_rev(false, None),
        ),
        rev_ug,
        json!({"mode": "ok"}),
        "rec-ug",
    )
    .await;
    let dead_obj = PluginGrant {
        capability: "read:sites".into(),
        scope: PluginGrantScope::Object {
            kind: "site".into(),
            id: format!("site:{}", SiteId::new()),
        },
        sensitive: false,
    };
    seed_claimed_staged_op(
        &store,
        StagedPluginOpKind::Upgrade,
        rev_ug,
        upg_intent(pid_ug, &up_ug2, &prep_ug, vec![dead_obj.clone()]),
        32,
    );
    api::recover_plugin_startup_for_test(&env.state).unwrap();
    assert!(
        store
            .get_plugin_version(pid_ug, up_ug2["hash"].as_str().unwrap())
            .unwrap()
            .is_none()
    );

    assert_resume_install_scope_and_gc(&env, dead_obj).await;
}

async fn resume_and_expect_failed(env: &HttpEnv, op: Uuid, rev: i64, idem: &str) {
    let (st, b) = post_write(
        env,
        format!("/api/v1/plugin-operations/{op}/resume"),
        rev,
        idem,
        json!({}),
    )
    .await;
    assert_eq!(assert_ok(st, &b, StatusCode::ACCEPTED)["state"], "failed");
}

async fn assert_resume_install_scope_and_gc(env: &HttpEnv, dead_obj: PluginGrant) {
    let store = env.state.store();
    let pid_ri1 = "org.example.r3-rec-inst1";
    let zi1 = task_pkg(pid_ri1, "1.0.0", &["read:sites"]);
    let up_i1 = env.upload_pkg(&zi1, None, "up-ri1").await;
    let h_i1 = up_i1["hash"].as_str().unwrap().to_string();
    let pre_pkg_dir = store.data_dir().join("plugins/packages").join(&h_i1);
    std::fs::create_dir_all(&pre_pkg_dir).unwrap();
    std::fs::write(pre_pkg_dir.join("package.cmplugin"), &zi1).unwrap();
    assert!(pre_pkg_dir.exists());

    let (op1, rev1) = seed_claimed_staged_op(
        &store,
        StagedPluginOpKind::Install,
        0,
        inst_intent(pid_ri1, &up_i1, vec![dead_obj]),
        33,
    );
    resume_and_expect_failed(env, op1, rev1, "res-ri1").await;
    assert!(store.get_plugin(pid_ri1).unwrap().is_none());
    assert!(!pre_pkg_dir.exists());
}

async fn sideload_while_disabling_dev_mode(env: &HttpEnv, pkg_bytes: Vec<u8>) -> String {
    let store = env.state.store();
    let pkg_hash = sha256_hex_of_pkg(&pkg_bytes);
    let hash_bg = pkg_hash.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        store
            .app_write(|app| {
                let _ = entered_tx.send(());
                let pkg_dir = store.data_dir().join("plugins/packages").join(&hash_bg);
                let deadline = Instant::now() + Duration::from_secs(5);
                while !pkg_dir.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                std::thread::sleep(Duration::from_millis(10));
                let cur = app.plugin_developer_mode()?;
                app.set_plugin_developer_mode(false, cur.revision, &admin_id().to_string())?;
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();
    let (st, _, b) = env.send(req_dev_raw(pkg_bytes)).await;
    handle.join().unwrap();
    assert_err(
        st,
        &b,
        StatusCode::FORBIDDEN,
        "plugin.developer_mode_disabled",
    );
    pkg_hash
}

#[tokio::test]
async fn slice_d_r3_item4_dev_sideload_rechecks_dev_mode_on_fresh_and_replace() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid_fresh = "org.example.r3-dev-fresh";
    let pkg_fresh =
        pkg::build_task_package_custom(pid_fresh, "1.0.0", 1, false, Some("f1"), false, &[]);
    let hash_fresh = sideload_while_disabling_dev_mode(&env, pkg_fresh).await;
    assert!(store.get_plugin(pid_fresh).unwrap().is_none());
    assert!(
        !store
            .data_dir()
            .join("plugins/packages")
            .join(&hash_fresh)
            .exists()
    );

    env.enable_dev_mode().await;
    let pid_rep = "org.example.r3-dev-replace";
    let pkg_v1 = pkg::build_task_package_custom(pid_rep, "1.0.0", 1, false, Some("v1"), false, &[]);
    let (st_v1, _, b_v1) = env.send(req_dev_raw(pkg_v1)).await;
    let hash_v1 = assert_ok(st_v1, &b_v1, st_v1)["package_hash"]
        .as_str()
        .unwrap()
        .to_string();

    let pkg_v2 = pkg::build_task_package_custom(pid_rep, "1.0.0", 1, false, Some("v2"), false, &[]);
    let _ = sideload_while_disabling_dev_mode(&env, pkg_v2).await;
    assert_eq!(
        store.get_plugin(pid_rep).unwrap().unwrap().package_hash,
        hash_v1
    );
}

async fn burst_identical_posts(
    env: &Arc<HttpEnv>,
    path: String,
    rev: i64,
    idem: String,
    body: Value,
) -> Vec<(StatusCode, Value)> {
    let delays_us: [u64; 4] = [0, 80, 250, 700];
    let mut set = tokio::task::JoinSet::new();
    for d in delays_us {
        let (ec, pc, ic, bc) = (env.clone(), path.clone(), idem.clone(), body.clone());
        set.spawn(async move {
            tokio::time::sleep(Duration::from_micros(d)).await;
            post_write(&ec, pc, rev, ic, bc).await
        });
    }
    let mut out = Vec::with_capacity(delays_us.len());
    while let Some(joined) = set.join_next().await {
        out.push(joined.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_d_r3_item5_concurrent_identical_retry_revoke_trust_key_and_enable_dev_mode() {
    let env = Arc::new(HttpEnv::new().await);

    for round in 0..4u64 {
        let (st_s, _, b_s) = env
            .send(req_json(
                "GET",
                "/api/v1/settings/plugins",
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let rev = assert_ok(st_s, &b_s, StatusCode::OK)["revision"]
            .as_i64()
            .unwrap();
        let reauth = env.reauth("developer-mode:enable").await;
        let replies = burst_identical_posts(
            &env,
            "/api/v1/settings/plugins/developer/enable".into(),
            rev,
            format!("r3-dev-idem-{round}"),
            json!({ "reauth_token": reauth }),
        )
        .await;
        for (st, b) in replies {
            assert_eq!(
                assert_ok(st, &b, StatusCode::OK)["revision"].as_i64(),
                Some(rev + 1)
            );
        }
        let (st_d, b_d) = post_write(
            &env,
            "/api/v1/settings/plugins/developer/disable",
            rev + 1,
            format!("r3-dev-dis-{round}"),
            json!({}),
        )
        .await;
        assert_ok(st_d, &b_d, StatusCode::ACCEPTED);
    }

    for round in 0..4u64 {
        let key = pkg::TestSigningKey::generate(9300 + round);
        let pinned = env.pin_trust_key(&key, &format!("r3-k-{round}")).await;
        let kid = pinned["key_id"].as_str().unwrap();
        let krev = pinned["revision"].as_i64().unwrap();
        let reauth = env.reauth("trust-key:revoke").await;
        let replies = burst_identical_posts(
            &env,
            format!("/api/v1/plugin-trust-keys/{kid}/revoke"),
            krev,
            format!("r3-rev-idem-{round}"),
            json!({ "reason": "rotate", "reauth_token": reauth }),
        )
        .await;
        let mut op_id: Option<String> = None;
        for (st, b) in replies {
            let cur = assert_ok(st, &b, StatusCode::ACCEPTED)["operation_id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(op_id.get_or_insert(cur.clone()), &cur);
        }
    }
}

#[tokio::test]
async fn slice_d_r3_item6_site_relative_disabled_site_and_boost_downloader_scope_trim() {
    let srv = support::StubTorrentServer::start(b"ok".to_vec());
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R3-Site");
    let dl_id = domain::DownloaderId::new();
    let store = env.state.store();
    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE sites SET url = ?1 WHERE id = ?2",
                rusqlite::params![format!("http://{}/", srv.addr()), site_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "R3-DL".into(),
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

    let pid = "org.example.r3-ports";
    let zip = task_pkg(pid, "1.0.0", &["http", "read:boost"]);
    let detail = env
        .install_pkg(
            &zip,
            None,
            json!([
                { "capability": "http", "scope": "Global", "sensitive": false },
                { "capability": "read:boost", "scope": { "Object": { "kind": "downloader", "id": format!("  {dl_id}  ") } }, "sensitive": false }
            ]),
        )
        .await;
    env.enable_plugin(pid, detail["revision"].as_i64().unwrap(), "r3-en-ports")
        .await;
    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let state = env.state.clone().with_plugin_http_policy_override(
        TargetPolicy::production().with_test_loopback_socket(srv.addr()),
    );
    let ports = state.plugins().host_ports();

    let mut inv_task = Invocation::new(pid, Duration::from_secs(5));
    inv_task.generation = u64::try_from(enabled.generation).unwrap();
    inv_task.role = GuestRole::Task;
    let _gt = ports.begin_invocation(&inv_task).unwrap();
    assert!(
        ports
            .boost_snapshot(&inv_task, &[], &[dl_id.to_string()])
            .is_ok()
    );

    let mut inv_site = Invocation::new(pid, Duration::from_secs(5));
    inv_site.generation = u64::try_from(enabled.generation).unwrap();
    inv_site.role = GuestRole::Task;
    inv_site.site_id = Some(site_id.to_string());
    let _gs = ports.begin_invocation(&inv_site).unwrap();

    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE sites SET enabled = 0 WHERE id = ?1",
                [site_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();

    let err = ports
        .http_fetch(
            &inv_site,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::SiteRelative(SiteTarget {
                    path: "/torrents.php".into(),
                    query: vec![],
                    auth_slot: None,
                }),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleGeneration);
    assert!(err.message.contains("disabled"));
}
