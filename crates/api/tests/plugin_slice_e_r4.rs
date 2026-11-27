#[path = "builtin_boost/support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api::plugins::SubmitAdminCommandInput;
use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::DownloaderId;
use downloader::{
    Downloader, MemoryDownloader, PathMap, SubmitOptions, SubmitPayload, TaskSnapshot,
};
use indexer::{
    Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse,
    RegistrySnapshot,
};
use serde_json::{Value, json};
use store::BoostTaskState;
use store::plugins::{CanonicalPluginRequest, PluginCommandTarget};
use support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, admin_id, assert_err, assert_ok, plugins_fixture, req_json,
};
use support::{NoopFetcher, poll_op};

struct CountingFakePage;

impl PluginBrowserPage for CountingFakePage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }
    fn navigate(&mut self, _: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![],
            body: vec![],
        })
    }
    fn eval_json(&mut self, _: &str) -> Result<Value, PluginBrowserError> {
        Ok(json!(true))
    }
    fn cookie_header(&mut self, _: &str) -> Result<String, PluginBrowserError> {
        Ok(String::new())
    }
    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        Ok(())
    }
}

fn make_counting_browser() -> (Arc<Browser>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();
    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(50))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(move |_| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CountingFakePage))
        }),
    );
    (browser, calls)
}

fn env_with_dl_maps(
    dir: tempfile::TempDir,
    dl: Arc<MemoryDownloader>,
    path_maps: Vec<PathMap>,
) -> (HttpEnv, domain::SiteId, DownloaderId) {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let site_id = env.insert_site("PT-R4");
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "R4DL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps,
            is_default: true,
            enabled: true,
        })
        .unwrap();
    (env, site_id, dl_id)
}

async fn run_boost_cmd(env: &HttpEnv, idem: &str, kind: &str, payload: Value) -> Value {
    let rev = env.get_plugin_rev("cm.boost").await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": kind, "payload": payload })),
        ))
        .await;
    let op = assert_ok(st, &body, StatusCode::ACCEPTED);
    poll_op(env, op["operation_id"].as_str().unwrap()).await
}

fn checkin_direct_input(
    site_id: domain::SiteId,
    rev: i64,
    idem: &str,
    tag: u8,
) -> SubmitAdminCommandInput {
    SubmitAdminCommandInput {
        actor_user_id: admin_id(),
        kind: "site.check-in".into(),
        payload: json!({ "site_id": site_id.to_string() }),
        targets: vec![PluginCommandTarget::Site(site_id)],
        evidence_ref: None,
        expected_revision: rev,
        idempotency_key: idem.into(),
        request: CanonicalPluginRequest {
            method: "POST".into(),
            path: "/api/v1/plugins/cm.checkin/commands".into(),
            body_digest: [tag; 32],
        },
    }
}

async fn post_checkin_cmd_while_app_writer_held(
    env: &HttpEnv,
    site_id: domain::SiteId,
    rev: i64,
    idem: &str,
    maintenance_gate: bool,
) -> (StatusCode, Value) {
    let store_bg = env.state.store();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        store_bg
            .app_write(|_| {
                let _ = ready_tx.send(());
                let _ = rel_rx.recv();
                Ok(())
            })
            .unwrap();
    });
    ready_rx.recv().unwrap();

    let req = req_json(
        "POST",
        "/api/v1/plugins/cm.checkin/commands",
        Some(TOKEN),
        &[
            ("if-match", &format!("\"{rev}\"")),
            ("idempotency-key", idem),
        ],
        Some(json!({
            "kind": "site.check-in",
            "payload": { "site_id": site_id.to_string() }
        })),
    );
    let mut fut = Box::pin(env.send(req));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut fut)
            .await
            .is_err(),
        "command request must wait on held Db::App writer"
    );
    let gate = env.state.write_gate();
    let closed = if maintenance_gate {
        Some(gate.close())
    } else {
        let _ = env.state.clone().with_instance_writable(false);
        None
    };
    let _ = rel_tx.send(());
    let (st, _, body) = fut.await;
    drop(closed);
    let _ = env.state.clone().with_instance_writable(true);
    holder.join().unwrap();
    (st, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slice_e_r4_item1_submit_admin_command_enforces_post_lock_write_admission() {
    let env = HttpEnv::new().await;
    plugins_fixture::install_cm_checkin(&env, false, false, false, false).await;
    let site_id = env.insert_site("PT-R4-1");
    let rev = env.get_plugin_rev("cm.checkin").await;
    let audit_before = env
        .state
        .store()
        .list_plugin_audit_events("cm.checkin", None, None, None, Some(100))
        .unwrap()
        .items
        .len();

    let gate = env.state.write_gate();
    {
        let _closed = gate.close();
        let err = env
            .state
            .plugins()
            .submit_admin_command(
                &env.state,
                "cm.checkin",
                checkin_direct_input(site_id, rev, "r4-direct-gate", 1),
            )
            .await
            .expect_err("direct submit_admin_command must reject when write_gate closed");
        assert_eq!(err.status_and_code(), (503, "runtime.busy"));
    }
    {
        let _ = env.state.clone().with_instance_writable(false);
        let err = env
            .state
            .plugins()
            .submit_admin_command(
                &env.state,
                "cm.checkin",
                checkin_direct_input(site_id, rev, "r4-direct-ro", 2),
            )
            .await
            .expect_err("direct submit_admin_command must reject when read-only");
        let _ = env.state.clone().with_instance_writable(true);
        assert_eq!(err.status_and_code(), (409, "instance.read_only"));
    }

    let (st_gate, b_gate) =
        post_checkin_cmd_while_app_writer_held(&env, site_id, rev, "r4-http-gate", true).await;
    assert_err(
        st_gate,
        &b_gate,
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime.busy",
    );

    let (st_ro, b_ro) =
        post_checkin_cmd_while_app_writer_held(&env, site_id, rev, "r4-http-ro", false).await;
    assert_err(st_ro, &b_ro, StatusCode::CONFLICT, "instance.read_only");

    let audit_after = env
        .state
        .store()
        .list_plugin_audit_events("cm.checkin", None, None, None, Some(100))
        .unwrap()
        .items
        .len();
    assert_eq!(
        audit_after, audit_before,
        "rejected admin commands must not write plugin_operations rows"
    );
}

#[tokio::test]
async fn slice_e_r4_item2_live_downloader_tasks_block_cleanup_and_preview() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let file_plan = stage.join("SharedPlanSeed.mkv");
    let file_ret = stage.join("SharedRetSeed.mkv");
    std::fs::write(&file_plan, b"shared-plan-payload").unwrap();
    std::fs::write(&file_ret, b"shared-ret-payload").unwrap();

    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl_maps(dir, dl.clone(), vec![]);
    plugins_fixture::install_cm_boost(&env, false, false, false).await;

    let tf_plan = plugins_fixture::seed_cm_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r4-plan",
        "1111222233334444555566667777888899990000",
        BoostTaskState::Seeding,
        19,
        0,
        Some(false),
        None,
    );
    let tf_ret = plugins_fixture::seed_cm_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r4-ret",
        "2222333344445555666677778888999900001111",
        BoostTaskState::Seeding,
        18,
        0,
        Some(false),
        None,
    );
    for (tid, f, sz) in [(&tf_plan, &file_plan, 19), (&tf_ret, &file_ret, 18)] {
        let pjson = json!({
            "save_path": stage,
            "allowed_roots": [stage],
            "paths": [f],
        })
        .to_string();
        env.state
            .store()
            .mark_boost_task_retained_with_paths(tid, 1, sz, Some(&pjson), 1_700_000_010)
            .unwrap();
    }

    let prep_ok = run_boost_cmd(
        &env,
        "r4-prep-before-live",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_plan] }),
    )
    .await;
    assert_eq!(prep_ok["state"], "succeeded");
    let plan_handle = prep_ok["result"]["plan_handle"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_rev = prep_ok["result"]["revision"].as_i64().unwrap();
    let (st_tok, _, b_tok) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle={plan_handle}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_tok, &b_tok, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    let live_hash = "9999888877776666555544443333222211110000";
    dl.submit_verified(
        &SubmitPayload::Magnet {
            uri: format!("magnet:?xt=urn:btih:{live_hash}"),
            info_hash: live_hash.into(),
        },
        &SubmitOptions {
            ownership_tag: "manual-live-seed".into(),
            ..Default::default()
        },
    )
    .unwrap();
    dl.set_task_files(
        live_hash,
        vec![
            PathBuf::from("SharedPlanSeed.mkv"),
            PathBuf::from("SharedRetSeed.mkv"),
        ],
        37,
    );
    dl.push_snapshot(TaskSnapshot {
        info_hash: live_hash.into(),
        name: "SharedBundle".into(),
        progress: 1.0,
        state: "uploading".into(),
        size_bytes: 37,
        ..Default::default()
    });

    assert_live_task_blocks_preview_and_cleanup(
        &env,
        &tf_ret,
        &plan_handle,
        plan_rev,
        &tok,
        (&file_plan, &file_ret),
    )
    .await;
}

async fn assert_live_task_blocks_preview_and_cleanup(
    env: &HttpEnv,
    tf_ret: &str,
    plan_handle: &str,
    plan_rev: i64,
    tok: &str,
    files: (&std::path::Path, &std::path::Path),
) {
    let (st_sel, _, b_sel) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?task_ids={tf_ret}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let sel = assert_ok(st_sel, &b_sel, StatusCode::OK);
    assert_eq!(sel["eligible_count"], 0);
    assert_eq!(sel["tasks"][0]["blocked_reason"], "shared-with-active-task");

    let (st_pl, _, b_pl) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle={plan_handle}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_pl, &b_pl, StatusCode::CONFLICT, "plugin.cleanup_blocked");

    let prep_blk = run_boost_cmd(
        env,
        "r4-prep-blocked",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_ret] }),
    )
    .await;
    assert_eq!(prep_blk["state"], "rejected");
    assert_eq!(prep_blk["phase"], "protection-denied:shared-path");

    let del_blk = run_boost_cmd(
        env,
        "r4-del-blocked",
        "downloader.delete-files",
        json!({
            "host_file_plan_handle": plan_handle,
            "revision": plan_rev,
            "confirmation_token": tok,
        }),
    )
    .await;
    assert_eq!(del_blk["state"], "rejected");
    assert_eq!(del_blk["phase"], "protection-denied:shared-path");
    assert!(
        files.0.exists() && files.1.exists(),
        "shared files must not be deleted while live task references them"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn slice_e_r4_item3_symlinked_ancestor_deletes_target_and_prunes_empty_subdir() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let real_root = dir.path().join("real_root");
    let sub_dir = real_root.join("nested_album");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let real_file = sub_dir.join("Track01.mkv");
    std::fs::write(&real_file, b"audio-video").unwrap();

    let sym_mount = dir.path().join("sym_mount");
    std::os::unix::fs::symlink(&real_root, &sym_mount).unwrap();
    let sym_target = sym_mount.join("nested_album/Track01.mkv");

    let dl = Arc::new(MemoryDownloader::new(real_root.clone()));
    let maps = vec![PathMap {
        from: "/dl".into(),
        to: real_root.clone(),
    }];
    let (env, site_id, dl_id) = env_with_dl_maps(dir, dl, maps);
    plugins_fixture::install_cm_boost(&env, false, false, false).await;

    let tf_id = plugins_fixture::seed_cm_boost_task(
        env.state.store().as_ref(),
        dl_id,
        site_id,
        "r4-sym-prune",
        "3333444455556666777788889999000011112222",
        BoostTaskState::Seeding,
        11,
        0,
        Some(false),
        None,
    );
    let pjson = json!({
        "save_path": sym_mount,
        "allowed_roots": [real_root],
        "paths": [sym_target],
    })
    .to_string();
    env.state
        .store()
        .mark_boost_task_retained_with_paths(&tf_id, 1, 11, Some(&pjson), 1_700_000_010)
        .unwrap();

    let prep = run_boost_cmd(
        &env,
        "r4-sym-prep",
        "downloader.prepare-cleanup",
        json!({ "task_ids": [tf_id] }),
    )
    .await;
    assert_eq!(prep["state"], "succeeded");
    let handle = prep["result"]["plan_handle"].as_str().unwrap();
    let rev = prep["result"]["revision"].as_i64().unwrap();

    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle={handle}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_p, &b_p, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    let del = run_boost_cmd(
        &env,
        "r4-sym-del",
        "downloader.delete-files",
        json!({
            "host_file_plan_handle": handle,
            "revision": rev,
            "confirmation_token": tok,
        }),
    )
    .await;
    assert_eq!(del["state"], "succeeded", "del={del}");
    assert!(!real_file.exists(), "real file must be unlinked");
    assert!(
        !sub_dir.exists(),
        "empty parent subdirectory under allowed_roots must be pruned"
    );
    assert!(real_root.exists(), "allowed_root itself must remain");
    assert!(
        sym_mount.symlink_metadata().is_ok(),
        "symlink mount outside allowed_root must remain untouched"
    );
}

#[tokio::test]
async fn slice_e_r4_item4_builtin_checkin_effective_grants_roundtrip_permissions_prepare() {
    let env = HttpEnv::new().await;
    plugins_fixture::install_cm_checkin(&env, false, false, false, false).await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.checkin/permissions",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let perms = assert_ok(st_g, &b_g, StatusCode::OK);
    let effective = perms["effective_grants"].as_array().unwrap();
    assert!(
        !effective
            .iter()
            .any(|g| g["capability"] == "maintenance:provide"),
        "cm.checkin must not bootstrap undeclared maintenance:provide: {effective:?}"
    );

    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": perms["effective_grants"] })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::OK);
    assert!(
        prep["consent_token"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
}

async fn post_browser_check(env: &HttpEnv, site_id: &str, rev: i64, idem: &str) -> Value {
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/browser-checks",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(json!({ "site_id": site_id, "purpose": "maintenance" })),
        ))
        .await;
    assert_ok(st, &body, StatusCode::ACCEPTED)
}

#[tokio::test]
async fn slice_e_r4_item5_browser_object_site_scope_ui_attribution_and_probe_gates() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (browser, cdp_calls) = make_counting_browser();
    let state = support::plugin_http_support::build_state(dir.path())
        .with_browser(browser)
        .with_admin_trusted_plugins(["cm.checkin"]);
    state.store().put_setting("cdp.enabled", "true").unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    plugins_fixture::install_cm_checkin(&env, true, true, false, true).await;

    let (st_s, _, b_s) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "HttpPreSwitchSite",
                "url": "https://pt.example",
                "profile_id": "demo",
                "maintenance_via": "http",
                "timezone": "UTC"
            })),
        ))
        .await;
    let site_id = assert_ok(st_s, &b_s, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let checkin_rec = env.state.store().get_plugin("cm.checkin").unwrap().unwrap();
    let rev0 = checkin_rec.revision;
    let gen0 = checkin_rec.generation.to_string();
    let (st_ui, _, b_ui) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/browser-checks",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev0}\"")),
                ("idempotency-key", "r4-bchk-ui"),
                ("x-plugin-package", &checkin_rec.package_hash),
                ("x-plugin-generation", &gen0),
            ],
            Some(json!({ "site_id": site_id, "purpose": "maintenance" })),
        ))
        .await;
    assert_err(
        st_ui,
        &b_ui,
        StatusCode::FORBIDDEN,
        "plugin.capability_denied",
    );

    let op_mg = post_browser_check(&env, &site_id, rev0, "r4-bchk-mg").await;
    assert_eq!(op_mg["state"], "failed");
    assert_eq!(op_mg["phase"], "missing-grant");
    assert_eq!(op_mg["result"]["reason"], "missing_grant");
    assert_eq!(cdp_calls.load(Ordering::SeqCst), 0);

    env.disable_plugin("cm.checkin", rev0, "r4-dis-chk").await;
    let rev_dis = env.get_plugin_rev("cm.checkin").await;
    let op_ne = post_browser_check(&env, &site_id, rev_dis, "r4-bchk-ne").await;
    assert_eq!(op_ne["state"], "failed");
    assert_eq!(op_ne["phase"], "plugin-not-enabled");
    assert_eq!(op_ne["result"]["reason"], "plugin_not_enabled");
    assert_eq!(cdp_calls.load(Ordering::SeqCst), 0);

    grant_object_site_browser_maint_and_enable(&env, &site_id).await;
    assert_object_scope_browser_ready_and_settings_gated(&env, &site_id, &cdp_calls).await;
}

async fn grant_object_site_browser_maint_and_enable(env: &HttpEnv, site_id: &str) {
    let rev_dis = env.get_plugin_rev("cm.checkin").await;
    let grants = json!([{
        "capability": "browser:maintenance",
        "scope": { "Object": { "kind": "site", "id": format!("site:{site_id}") } },
        "sensitive": true
    }]);
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::OK);
    let (st_gr, _, b_gr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/permissions/grant",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "r4-obj-gr"),
            ],
            Some(json!({ "consent_token": prep["consent_token"], "grants": grants })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_gr, &b_gr, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
    let rev_gr = env.get_plugin_rev("cm.checkin").await;
    env.enable_plugin("cm.checkin", rev_gr, "r4-en-chk").await;
}

async fn assert_object_scope_browser_ready_and_settings_gated(
    env: &HttpEnv,
    site_id: &str,
    cdp_calls: &AtomicUsize,
) {
    let (st_av, _, b_av) = env
        .send(req_json(
            "GET",
            &format!(
                "/api/v1/plugins/cm.checkin/browser-availability?site_id={site_id}&purpose=maintenance"
            ),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let av = assert_ok(st_av, &b_av, StatusCode::OK);
    assert_eq!(av["grant_present"], true, "av={av}");
    assert_eq!(av["site_enabled"], false);

    let rev_en = env.get_plugin_rev("cm.checkin").await;
    let op_ok = post_browser_check(env, site_id, rev_en, "r4-bchk-ok").await;
    assert_eq!(op_ok["state"], "succeeded", "op_ok={op_ok}");
    assert_eq!(op_ok["phase"], "checked");
    assert_eq!(cdp_calls.load(Ordering::SeqCst), 1);

    env.state
        .store()
        .put_setting("cdp.enabled", "false")
        .unwrap();
    let rev_en2 = env.get_plugin_rev("cm.checkin").await;
    let op_cd = post_browser_check(env, site_id, rev_en2, "r4-bchk-cdp-off").await;
    assert_eq!(op_cd["state"], "failed");
    assert_eq!(op_cd["phase"], "cdp-disabled");
    assert_eq!(cdp_calls.load(Ordering::SeqCst), 1);
}
