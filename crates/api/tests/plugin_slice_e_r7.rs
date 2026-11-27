#[path = "builtin_boost/support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api::plugins::reconcile_pending;
use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::DownloaderId;
use downloader::{Downloader, MemoryDownloader, SubmitOptions, SubmitPayload, torrent_info_hash};
use indexer::RegistrySnapshot;
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginCommandOutcome,
    PluginEffectObservation, PluginOperationState, PluginSubject, StagedPluginIntent,
    StagedPluginOpKind,
};
use store::{BoostTaskState, PrepareBoostCleanupPlanInput};
use support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};
use support::{NoopFetcher, poll_op};
use uuid::Uuid;

async fn install_and_prepare_v2(env: &HttpEnv, pid: &str, tag: u8) -> (Value, Option<String>) {
    let zip_v1 = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let inst = env
        .install_pkg(
            &zip_v1,
            None,
            json!([{"capability": "kv", "scope": "Global", "sensitive": false}]),
        )
        .await;
    let (_, _, cfg_get) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/config"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = cfg_get["data"]["schema_revision"].as_str().unwrap();
    let (st_cfg, _, b_cfg) = env
        .send(req_json(
            "PUT",
            &format!("/api/v1/plugins/{pid}/config"),
            Some(TOKEN),
            &[
                (
                    "if-match",
                    &format!("\"{}\"", inst["revision"].as_i64().unwrap()),
                ),
                ("idempotency-key", &format!("r7-cfg-v1-{tag}")),
            ],
            Some(json!({ "schema_revision": srev, "values": { "mode": "fast" } })),
        ))
        .await;
    assert_ok(st_cfg, &b_cfg, StatusCode::OK);
    let rev_en = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev_en, &format!("r7-en-{tag}"))
        .await;

    let zip_v2 = pkg::build_task_package_custom(pid, "1.1.0", 1, false, None, false, &["kv"]);
    let up_v2 = env
        .upload_pkg(&zip_v2, None, &format!("r7-up-v2-{tag}"))
        .await;
    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{}\"", cur.revision)),
                ("idempotency-key", &format!("r7-prep-v2-{tag}")),
            ],
            Some(json!({
                "version": "1.1.0",
                "upload_id": up_v2["upload_id"],
                "state_policy": "preserve",
                "target_config": {
                    "schema_revision": srev,
                    "values": { "mode": "fast" },
                    "secrets": {}
                }
            })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED);
    let cfg_digest = Some(
        prep["result"]["target_config_digest"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    (up_v2, cfg_digest)
}

async fn seed_draining_preserve_upgrade(
    env: &HttpEnv,
    pid: &str,
    tag: u8,
) -> (Uuid, i64, Box<dyn Send>) {
    let (up_v2, cfg_digest) = install_and_prepare_v2(env, pid, tag).await;
    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    let inv = Invocation::new(pid, Duration::from_secs(30));
    let guard: Box<dyn Send> = Box::new(
        env.state
            .plugins()
            .enter_library_invocation_for_test(pid, cur.generation, &inv)
            .unwrap(),
    );

    let intent = StagedPluginIntent {
        target_plugin_id: pid.into(),
        package_hash: Some(up_v2["hash"].as_str().unwrap().into()),
        version: Some("1.1.0".into()),
        artifact_path: Some(format!(
            "plugins/uploads/{}/package.cmplugin",
            up_v2["upload_id"].as_str().unwrap()
        )),
        state_policy: Some("preserve".into()),
        target_config_digest: cfg_digest,
        grants: vec![store::plugins::PluginGrant {
            capability: "kv".into(),
            scope: store::plugins::PluginGrantScope::Global,
            sensitive: false,
        }],
    };
    let (op_id, rev) = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("r7-seed-upg-{tag}"),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: "/plugin-operations/op/resume".into(),
                    body_digest: [tag; 32],
                },
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur.revision,
                intent,
                cause: PluginCommandCause::root(format!("r7-upg:{tag}")),
            })?;
            let op_id = acc.operation.operation_id;
            let claim = app.claim_staged_plugin_operation(
                op_id,
                acc.operation.revision,
                0,
                store::unix_now(),
            )?;
            let _ = app.begin_plugin_lifecycle(pid, cur.revision, op_id, false)?;
            let blocked = app.record_plugin_command_outcome(
                &claim,
                &PluginCommandOutcome {
                    state: PluginOperationState::Blocked,
                    phase: "drain-pending".into(),
                    next_run_at: None,
                    result: None,
                    effect: PluginEffectObservation::NoEffect,
                },
            )?;
            Ok((op_id, blocked.revision))
        })
        .unwrap();
    (op_id, rev, guard)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staged_resume_enforces_admission_on_rollforward_and_allows_idempotent_retry_at_original_if_match()
 {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.r7-resume-ro";
    let (u_op, u_rev, guard) = seed_draining_preserve_upgrade(&env, pid, 71).await;

    let mk_resume = move || {
        req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{u_op}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{u_rev}\"")),
                ("idempotency-key", "r7-resume-idem-1"),
            ],
            Some(json!({ "evidence_ref": "retry-safe" })),
        )
    };

    let app_clone = env.app.clone();
    let first_fut = tokio::spawn(async move {
        use tower::ServiceExt;
        let resp = app_clone.oneshot(mk_resume()).await.unwrap();
        let st = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (st, serde_json::from_slice::<Value>(&bytes).unwrap())
    });

    for _ in 0..50 {
        let cur = env
            .state
            .store()
            .get_plugin_operation_admin(u_op)
            .unwrap()
            .unwrap();
        if cur.operation.revision > u_rev {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Background reconcile_pending must NOT steal an active foreground resume claim!
    drop(guard);
    let _ = env.state.clone().with_instance_writable(false);
    let _ = reconcile_pending(&env.state, store::unix_now());

    let (st1, b1) = first_fut.await.unwrap();
    let _ = env.state.clone().with_instance_writable(true);

    // Must be rejected with 409 instance.read_only (NOT 202 succeeded while read-only, and NOT permanently failed!).
    assert_err(st1, &b1, StatusCode::CONFLICT, "instance.read_only");
    let mid = env
        .state
        .store()
        .get_plugin_operation_admin(u_op)
        .unwrap()
        .unwrap();
    assert_ne!(
        mid.operation.state,
        PluginOperationState::Failed,
        "read-only refusal must NOT permanently fail preserve upgrade"
    );

    // Retrying with the ORIGINAL If-Match (`u_rev`) and SAME Idempotency-Key must succeed!
    let (st2, _, b2) = env.send(mk_resume()).await;
    let v2 = assert_ok(st2, &b2, StatusCode::ACCEPTED);
    assert_eq!(v2["state"], "succeeded", "v2={v2}");

    // Replaying the same Idempotency-Key a third time replays the succeeded view cleanly.
    let (st3, _, b3) = env.send(mk_resume()).await;
    let v3 = assert_ok(st3, &b3, StatusCode::ACCEPTED);
    assert_eq!(v3, v2);
}

fn setup_r7_sibling_boost_env() -> (
    HttpEnv,
    PathBuf,
    domain::SiteId,
    Arc<MemoryDownloader>,
    String,
) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let shared_file = stage.join("SharedManual.mkv");
    std::fs::write(&shared_file, b"shared-payload").unwrap();

    let dl_mem = Arc::new(MemoryDownloader::new(stage.clone()));
    let torrent_bytes = {
        let info = b"d6:lengthi14e4:name16:SharedManual.mkv12:piece lengthi16384e6:pieces20:01234567890123456789e";
        let mut out = b"d8:announce19:http://127.0.0.1/an4:info".to_vec();
        out.extend_from_slice(info);
        out.push(b'e');
        out
    };
    let info_hash = torrent_info_hash(&torrent_bytes).unwrap();
    dl_mem
        .submit_verified(
            &SubmitPayload::TorrentBytes {
                bytes: torrent_bytes,
                bytes_digest: "dig-manual-1".into(),
                info_hash: info_hash.clone(),
            },
            &SubmitOptions {
                ownership_tag: "cm-op:manual-1".into(),
                save_path: Some(stage.to_string_lossy().into_owned()),
                category: None,
                extra_tags: vec![],
                paused: false,
                upload_limit: None,
                download_limit: None,
                sequential: false,
                first_last_piece: false,
            },
        )
        .unwrap();

    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(support::plugin_http_support::ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl_mem.clone() as Arc<dyn Downloader>,
    )
    .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    let site_id = env.insert_site("PT-R7-Sib");

    let dl_default = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_default,
            name: "DefaultLiveManual".into(),
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

    (env, shared_file, site_id, dl_mem, info_hash)
}

fn seed_disabled_retained_plan(
    env: &HttpEnv,
    stage: &std::path::Path,
    shared_file: &std::path::Path,
    site_id: domain::SiteId,
) -> i64 {
    let dl_disabled = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_disabled,
            name: "DisabledUnreachableTransmission".into(),
            kind: "transmission".into(),
            url: "http://127.0.0.1:9/transmission/rpc".into(),
            username: Some("u".into()),
            password: Some("p".into()),
            api_key: None,
            category: None,
            path_maps: vec![downloader::PathMap {
                from: "/dl".into(),
                to: PathBuf::from(stage),
            }],
            is_default: false,
            enabled: false,
        })
        .unwrap();

    let tf = support::plugin_http_support::plugins_fixture::seed_cm_boost_task(
        env.state.store().as_ref(),
        dl_disabled,
        site_id,
        "r7-sib-tag",
        "3030303030303030303030303030303030303030",
        BoostTaskState::Seeding,
        14,
        0,
        Some(false),
        None,
    );
    let pjson = json!({
        "save_path": stage,
        "allowed_roots": [stage],
        "paths": [shared_file],
    })
    .to_string();
    let ret = env
        .state
        .store()
        .mark_boost_task_retained_with_paths(&tf, 1, 14, Some(&pjson), 1_700_000_050)
        .unwrap();
    let planned = env
        .state
        .store()
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf,
            expected_revision: ret.revision,
            cleanup_plan_handle: "r7-sib-plan".into(),
            cleanup_plan_json: pjson,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    planned.revision
}

#[tokio::test]
async fn boost_cleanup_protects_live_files_on_enabled_downloader_without_boost_facts_and_skips_disabled_downloaders()
 {
    let (env, shared_file, site_id, dl_mem, info_hash) = setup_r7_sibling_boost_env();
    support::plugin_http_support::plugins_fixture::install_cm_boost(&env, false, false, false)
        .await;
    let stage = shared_file.parent().unwrap().to_path_buf();
    let planned_rev = seed_disabled_retained_plan(&env, &stage, &shared_file, site_id);

    // 1) Preview skips disabled `dl_disabled` (127.0.0.1:9) and mints token before manual task starts.
    let (st_prev1, _, b_prev1) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=r7-sib-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let prev1 = assert_ok(st_prev1, &b_prev1, StatusCode::OK);
    let tok = prev1["confirmation_token"].as_str().unwrap().to_string();

    // 2) Enabled `dl_default` (0 boost_task_facts) starts a live manual task on `shared_file`:
    //    preview now blocks with `plugin.cleanup_blocked` AND `delete-files` refuses to unlink `shared_file`!
    dl_mem.set_task_files(&info_hash, vec![PathBuf::from("SharedManual.mkv")], 14);
    dl_mem.push_snapshot(downloader::TaskSnapshot {
        tag: String::new(),
        name: "SharedManual.mkv".into(),
        progress: 1.0,
        state: "seeding".into(),
        size_bytes: 14,
        downloaded_bytes: 14,
        uploaded_bytes: 0,
        download_speed: 0,
        upload_speed: 0,
        info_hash,
        owned_tags: vec!["cm-op:manual-1".into()],
        ..Default::default()
    });
    let (st_prev2, _, b_prev2) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle=r7-sib-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_prev2,
        &b_prev2,
        StatusCode::CONFLICT,
        "plugin.cleanup_blocked",
    );

    let rev = env.get_plugin_rev("cm.boost").await;
    let (st_del, _, b_del) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", "r7-del-shared"),
            ],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": "r7-sib-plan",
                    "revision": planned_rev,
                    "confirmation_token": tok,
                }
            })),
        ))
        .await;
    let op = assert_ok(st_del, &b_del, StatusCode::ACCEPTED);
    let settled = poll_op(&env, op["operation_id"].as_str().unwrap()).await;
    assert_ne!(
        settled["phase"], "read-protection-failed",
        "disabled downloader must be skipped without failing protection check: {settled}"
    );
    assert!(
        shared_file.exists(),
        "file actively used by another enabled downloader (even with 0 boost_task_facts) must NOT be unlinked!"
    );
}
