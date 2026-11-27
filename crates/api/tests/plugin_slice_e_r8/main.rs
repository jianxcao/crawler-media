#[path = "../builtin_boost/support.rs"]
pub(crate) mod support;

mod browser;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, DownloaderEnv, DynamicDownloader, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SiteId};
use downloader::{
    Downloader, MemoryDownloader, SubmitOptions, SubmitPayload, TaskSnapshot, torrent_info_hash,
};
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
use support::{NoopFetcher, poll_op, seed_boost_task};
use uuid::Uuid;

fn body_sha256(body: &Value) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(body).unwrap());
    h.finalize().into()
}

async fn seed_queued_claimed_install(
    env: &HttpEnv,
    pid: &str,
    idem_claim: &str,
    claim_body: &Value,
) -> (Uuid, i64, i64) {
    let zip = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let up = env.upload_pkg(&zip, None, &format!("up-{pid}")).await;
    let body_digest = body_sha256(claim_body);
    env.state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("seed-{pid}"),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: "/plugins/install".into(),
                    body_digest: [82; 32],
                },
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(up["hash"].as_str().unwrap().into()),
                    version: Some("1.0.0".into()),
                    artifact_path: Some(format!(
                        "plugins/uploads/{}/package.cmplugin",
                        up["upload_id"].as_str().unwrap()
                    )),
                    state_policy: None,
                    target_config_digest: None,
                    grants: vec![store::plugins::PluginGrant {
                        capability: "kv".into(),
                        scope: store::plugins::PluginGrantScope::Global,
                        sensitive: false,
                    }],
                },
                cause: PluginCommandCause::root(format!("seed:{pid}")),
            })?;
            let orig_rev = acc.operation.revision;
            let claim = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                orig_rev,
                0,
                store::unix_now(),
            )?;
            let meta = json!({
                "actor": admin_id().to_string(),
                "idempotency_key": idem_claim,
                "method": "POST",
                "path": format!("/plugin-operations/{}/resume", acc.operation.operation_id),
                "body_digest": body_digest,
                "original_revision": orig_rev,
                "claimed_revision": claim.revision(),
            });
            app.put_setting(
                &format!("plugin_op.resume_claim.{}", acc.operation.operation_id),
                &meta.to_string(),
            )?;
            Ok((acc.operation.operation_id, orig_rev, claim.revision()))
        })
        .unwrap()
}

#[tokio::test]
async fn queued_staged_resume_retries_on_accepted_phase_and_guards_mismatch_and_cancel() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let same_body = json!({"evidence_ref": "same"});
    let (op_id, rev0, _) =
        seed_queued_claimed_install(&env, "org.example.r8-qr-retry", "r8-qr-idem", &same_body)
            .await;

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev0}\"")),
                ("idempotency-key", "r8-qr-idem"),
            ],
            Some(json!({"evidence_ref": "MUTATED"})),
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );

    let (st_ok, _, b_ok) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev0}\"")),
                ("idempotency-key", "r8-qr-idem"),
            ],
            Some(same_body.clone()),
        ))
        .await;
    let view_ok = assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);
    assert_eq!(view_ok["state"], "succeeded");

    let (op_cancel, _, claimed_rev) = seed_queued_claimed_install(
        &env,
        "org.example.r8-qr-cancel",
        "r8-qr-cancel-idem",
        &same_body,
    )
    .await;
    let (st_can, _, b_can) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_cancel}/cancel"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{claimed_rev}\"")),
                ("idempotency-key", "r8-do-cancel"),
            ],
            Some(json!({"reason": "abort"})),
        ))
        .await;
    assert_ok(st_can, &b_can, StatusCode::ACCEPTED);
    assert!(
        env.state
            .store()
            .get_setting(&format!("plugin_op.resume_claim.{op_cancel}"))
            .unwrap()
            .is_none(),
        "cancel_operation must clean up plugin_op.resume_claim setting"
    );
}

async fn install_and_prepare_upgrade_v2(
    env: &HttpEnv,
    pid: &str,
) -> (serde_json::Value, Option<String>) {
    let zip1 = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let inst = env
        .install_pkg(
            &zip1,
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
                ("idempotency-key", "r8-cr-cfg"),
            ],
            Some(json!({ "schema_revision": srev, "values": { "mode": "fast" } })),
        ))
        .await;
    assert_ok(st_cfg, &b_cfg, StatusCode::OK);
    let rev_en = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev_en, "r8-cr-en").await;

    let zip2 = pkg::build_task_package_custom(pid, "1.1.0", 1, false, None, false, &["kv"]);
    let up2 = env.upload_pkg(&zip2, None, "r8-cr-up2").await;
    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{}\"", cur.revision)),
                ("idempotency-key", "r8-cr-prep"),
            ],
            Some(json!({
                "version": "1.1.0",
                "upload_id": up2["upload_id"],
                "state_policy": "preserve",
                "target_config": { "schema_revision": srev, "values": { "mode": "fast" }, "secrets": {} }
            })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::ACCEPTED);
    let cfg_digest = prep["result"]["target_config_digest"]
        .as_str()
        .map(str::to_string);
    (up2, cfg_digest)
}

async fn seed_draining_upgrade_op(env: &HttpEnv, pid: &str) -> (Uuid, i64, Box<dyn Send>) {
    let (up2, cfg_digest) = install_and_prepare_upgrade_v2(env, pid).await;
    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();

    let inv = Invocation::new(pid, Duration::from_secs(30));
    let guard: Box<dyn Send> = Box::new(
        env.state
            .plugins()
            .enter_library_invocation_for_test(pid, cur.generation, &inv)
            .unwrap(),
    );
    let (op_id, blocked_rev) = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r8-cr-seed".into(),
                request: CanonicalPluginRequest {
                    method: "POST".into(),
                    path: format!("/plugins/{pid}/upgrades"),
                    body_digest: [85; 32],
                },
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur.revision,
                intent: StagedPluginIntent {
                    target_plugin_id: pid.into(),
                    package_hash: Some(up2["hash"].as_str().unwrap().into()),
                    version: Some("1.1.0".into()),
                    artifact_path: Some(format!(
                        "plugins/uploads/{}/package.cmplugin",
                        up2["upload_id"].as_str().unwrap()
                    )),
                    state_policy: Some("preserve".into()),
                    target_config_digest: cfg_digest,
                    grants: vec![store::plugins::PluginGrant {
                        capability: "kv".into(),
                        scope: store::plugins::PluginGrantScope::Global,
                        sensitive: false,
                    }],
                },
                cause: PluginCommandCause::root("r8-cr"),
            })?;
            let oid = acc.operation.operation_id;
            let claim = app.claim_staged_plugin_operation(
                oid,
                acc.operation.revision,
                0,
                store::unix_now(),
            )?;
            let _ = app.begin_plugin_lifecycle(pid, cur.revision, oid, false)?;
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
            Ok((oid, blocked.revision))
        })
        .unwrap();
    (op_id, blocked_rev, guard)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_staged_resume_with_different_idempotency_key_cannot_steal_active_claim() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (op_id, blocked_rev, guard) =
        seed_draining_upgrade_op(&env, "org.example.r8-conc-resume").await;

    let app1 = env.app.clone();
    let fut1 = tokio::spawn(async move {
        use tower::ServiceExt;
        let resp = app1
            .oneshot(req_json(
                "POST",
                &format!("/api/v1/plugin-operations/{op_id}/resume"),
                Some(TOKEN),
                &[
                    ("if-match", &format!("\"{blocked_rev}\"")),
                    ("idempotency-key", "r8-cr-key-1"),
                ],
                Some(json!({})),
            ))
            .await
            .unwrap();
        let st = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (st, serde_json::from_slice::<Value>(&bytes).unwrap())
    });

    for _ in 0..50 {
        let cur_op = env
            .state
            .store()
            .get_plugin_operation_admin(op_id)
            .unwrap()
            .unwrap();
        if cur_op.operation.revision > blocked_rev {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let next_rev = blocked_rev + 1;
    let (st2, _, b2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{next_rev}\"")),
                ("idempotency-key", "r8-cr-key-2"),
            ],
            Some(json!({})),
        ))
        .await;
    assert_eq!(
        st2,
        StatusCode::CONFLICT,
        "concurrent resume must not steal claim: {b2}"
    );

    drop(guard);
    let (st1, b1) = fut1.await.unwrap();
    let v1 = assert_ok(st1, &b1, StatusCode::ACCEPTED);
    assert_eq!(
        v1["state"], "succeeded",
        "first resume must finish cleanly: {v1}"
    );
}

#[allow(clippy::too_many_arguments)]
fn seed_retained_cleanup_plan(
    store: &Store,
    dl_id: DownloaderId,
    site_id: SiteId,
    tag: &str,
    info_hash: &str,
    stage: &std::path::Path,
    target_file: &std::path::Path,
    plan_handle: &str,
    size_bytes: i64,
) -> i64 {
    let tf = seed_boost_task(
        store,
        dl_id,
        site_id,
        tag,
        info_hash,
        BoostTaskState::Seeding,
        size_bytes,
        0,
        Some(false),
        None,
    );
    let pjson = json!({
        "save_path": stage,
        "allowed_roots": [stage],
        "paths": [target_file],
    })
    .to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(&tf, 1, size_bytes, Some(&pjson), 1_700_000_050)
        .unwrap();
    store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf,
            expected_revision: ret.revision,
            cleanup_plan_handle: plan_handle.into(),
            cleanup_plan_json: pjson,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap()
        .revision
}

fn seed_live_core_torrent(env_dl: &MemoryDownloader, dl_root: &std::path::Path, file_name: &str) {
    let info = format!(
        "d6:lengthi15e4:name{}:{file_name}12:piece lengthi16384e6:pieces20:01234567890123456789e",
        file_name.len()
    );
    let mut torrent_bytes = b"d8:announce19:http://127.0.0.1/an4:info".to_vec();
    torrent_bytes.extend_from_slice(info.as_bytes());
    torrent_bytes.push(b'e');
    let live_hash = torrent_info_hash(&torrent_bytes).unwrap();
    env_dl
        .submit_verified(
            &SubmitPayload::TorrentBytes {
                bytes: torrent_bytes,
                bytes_digest: "dig-r8-core-1".into(),
                info_hash: live_hash.clone(),
            },
            &SubmitOptions {
                ownership_tag: "cm-op:core-1".into(),
                save_path: Some(dl_root.to_string_lossy().into_owned()),
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
    env_dl.set_task_files(&live_hash, vec![PathBuf::from(file_name)], 15);
    env_dl.push_snapshot(TaskSnapshot {
        tag: String::new(),
        name: file_name.into(),
        progress: 1.0,
        state: "seeding".into(),
        size_bytes: 15,
        downloaded_bytes: 15,
        uploaded_bytes: 0,
        download_speed: 0,
        upload_speed: 0,
        info_hash: live_hash,
        owned_tags: vec!["cm-op:core-1".into()],
    });
}

#[tokio::test]
async fn boost_protects_default_downloader_when_disabled_row_exists_and_skips_disabled_metrics() {
    let dir_a = tempfile::tempdir_in("/tmp").unwrap();
    let dl_root_a = dir_a.path().join("dl_a");
    std::fs::create_dir_all(&dl_root_a).unwrap();
    let env_dl = Arc::new(MemoryDownloader::new(dl_root_a.clone()));
    let store_a = Arc::new(Store::open(dir_a.path().join("data_a")).unwrap());
    let state_a = ApiState::new_arc(
        store_a,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        env_dl.clone() as Arc<dyn Downloader>,
    )
    .unwrap();
    let env_a = HttpEnv {
        _dir: dir_a,
        app: router(state_a.clone()),
        state: state_a,
        member_token: String::new(),
    };
    let site_a = env_a.insert_site("R8-Boost-Site-A");

    let disabled_dl = DownloaderId::new();
    env_a
        .state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: disabled_dl,
            name: "Decommissioned-qB".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: Some("admin".into()),
            password: Some("pw".into()),
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: false,
            enabled: false,
        })
        .unwrap();
    let shared_path = dl_root_a.join("shared_env.mkv");
    std::fs::write(&shared_path, b"live-core-bytes").unwrap();
    let _ = seed_retained_cleanup_plan(
        env_a.state.store().as_ref(),
        disabled_dl,
        site_a,
        "r8-dead-tag",
        "dead000000000000000000000000000000000001",
        &dl_root_a,
        &shared_path,
        "r8-env-plan",
        15,
    );

    let (st_bs, _, b_bs) = env_a
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-state",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let bs_data = assert_ok(st_bs, &b_bs, StatusCode::OK);
    assert_ne!(bs_data["disk"]["status"], "unavailable", "bs={bs_data}");

    seed_live_core_torrent(&env_dl, &dl_root_a, "shared_env.mkv");
    let (st_prev_a, _, b_prev_a) = env_a
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r8-env-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_prev_a,
        &b_prev_a,
        StatusCode::CONFLICT,
        "plugin.cleanup_blocked",
    );
}

#[tokio::test]
async fn boost_cleanup_succeeds_on_empty_dynamic_downloader_without_env_fallback() {
    let dir_b = tempfile::tempdir_in("/tmp").unwrap();
    let dl_root_b = dir_b.path().join("dl_b");
    std::fs::create_dir_all(&dl_root_b).unwrap();
    let retained_file = dl_root_b.join("orphan_boost.mkv");
    std::fs::write(&retained_file, b"reclaim-me").unwrap();
    let store_b = Arc::new(Store::open(dir_b.path().join("data_b")).unwrap());
    let dyn_dl = Arc::new(DynamicDownloader::new(
        store_b.clone(),
        DownloaderEnv::default(),
        dir_b.path(),
    ));
    let state_b = ApiState::new_arc(
        store_b,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dyn_dl as Arc<dyn Downloader>,
    )
    .unwrap();
    let env_b = HttpEnv {
        _dir: dir_b,
        app: router(state_b.clone()),
        state: state_b,
        member_token: String::new(),
    };
    let site_b = env_b.insert_site("R8-Boost-Site-B");
    let plan_rev = seed_retained_cleanup_plan(
        env_b.state.store().as_ref(),
        DownloaderId::new(),
        site_b,
        "r8-dyn-tag",
        "cccc111122223333444455556666777788889999",
        &dl_root_b,
        &retained_file,
        "r8-dyn-plan",
        10,
    );
    let (st_prev_b, _, b_prev_b) = env_b
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r8-dyn-plan",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let prev_b = assert_ok(st_prev_b, &b_prev_b, StatusCode::OK);
    let tok_b = prev_b["confirmation_token"].as_str().unwrap();
    let rev_b = env_b.get_plugin_rev("builtin.boost").await;
    let (st_del_b, _, b_del_b) = env_b
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_b}\"")),
                ("idempotency-key", "r8-dyn-del"),
            ],
            Some(json!({
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": "r8-dyn-plan",
                    "revision": plan_rev,
                    "confirmation_token": tok_b
                }
            })),
        ))
        .await;
    let op_b = assert_ok(st_del_b, &b_del_b, StatusCode::ACCEPTED);
    let settled_b = poll_op(&env_b, op_b["operation_id"].as_str().unwrap()).await;
    assert_eq!(settled_b["state"], "succeeded", "settled_b={settled_b}");
    assert!(!retained_file.exists());
}
