use std::sync::Arc;
use std::time::Duration;

use api::plugins::{CancelOperationInput, ResumeOperationInput};
use axum::http::StatusCode;
use domain::PluginOperationId;
use downloader::MemoryDownloader;
use serde_json::{Value, json};
use store::BoostTaskState;
use store::PrepareBoostCleanupPlanInput;
use store::plugins::{
    AcceptPluginOperation, AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause,
    PluginCommandOutcome, PluginCommandTarget, PluginEffectObservation, PluginOperationState,
    PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};
use uuid::Uuid;

use crate::support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};
use crate::support::seed_boost_task;
use crate::{env_with_dl_maps, http_canon_req};

fn dummy_req(tag: u8) -> CanonicalPluginRequest {
    CanonicalPluginRequest {
        method: "POST".into(),
        path: "/plugin-operations/op".into(),
        body_digest: [tag; 32],
    }
}

fn seed_staged_install_op(
    env: &HttpEnv,
    pid: &str,
    up: &Value,
    tag: u8,
    terminal: bool,
) -> (Uuid, i64) {
    let intent = StagedPluginIntent {
        target_plugin_id: pid.into(),
        package_hash: Some(up["hash"].as_str().unwrap().into()),
        version: Some("1.0.0".into()),
        artifact_path: Some(format!(
            "plugins/uploads/{}/package.cmplugin",
            up["upload_id"].as_str().unwrap()
        )),
        state_policy: None,
        target_config_digest: None,
        grants: vec![],
    };
    env.state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("r5-st-{tag}"),
                request: dummy_req(tag),
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent,
                cause: PluginCommandCause::root(format!("r5-st:{tag}")),
            })?;
            if !terminal {
                return Ok((acc.operation.operation_id, acc.operation.revision));
            }
            let claim = app.claim_staged_plugin_operation(
                acc.operation.operation_id,
                acc.operation.revision,
                0,
                1_700_000_000,
            )?;
            let done = app.complete_staged_plugin_operation(
                &claim,
                PluginOperationState::Failed,
                "pre-failed",
                None,
                Some("pre-failed"),
                1_700_000_001,
            )?;
            Ok((done.operation_id, done.revision))
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r5_item1a_cancel_and_resume_enforce_write_admission_and_reject_terminal_staged_op()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let fake_op = Uuid::new_v4();

    let gate = env.state.write_gate();
    {
        let _closed = gate.close();
        let c_err = env
            .state
            .plugins()
            .cancel_operation(
                &env.state,
                fake_op,
                CancelOperationInput {
                    actor_user_id: admin_id(),
                    reason: None,
                    expected_revision: 1,
                    idempotency_key: "r5-c-gate".into(),
                    request: dummy_req(1),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(c_err.status_and_code(), (503, "runtime.busy"));
        let r_err = env
            .state
            .plugins()
            .resume_operation(
                &env.state,
                fake_op,
                ResumeOperationInput {
                    actor_user_id: admin_id(),
                    expected_revision: 1,
                    evidence_ref: None,
                    idempotency_key: "r5-r-gate".into(),
                    request: dummy_req(2),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(r_err.status_and_code(), (503, "runtime.busy"));
    }

    let zip1 = pkg::build_task_package_custom(
        "org.example.r5-term-st",
        "1.0.0",
        1,
        false,
        None,
        false,
        &["kv"],
    );
    let up1 = env.upload_pkg(&zip1, None, "up-r5-st1").await;
    let (term_op, term_rev) =
        seed_staged_install_op(&env, "org.example.r5-term-st", &up1, 10, true);
    let (st_term, _, b_term) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{term_op}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{term_rev}\"")),
                ("idempotency-key", "r5-res-term"),
            ],
            Some(json!({})),
        ))
        .await;
    assert_err(st_term, &b_term, StatusCode::CONFLICT, "plugin.conflict");

    assert_staged_resume_enforces_post_lock_admission(&env).await;
}

async fn assert_staged_resume_enforces_post_lock_admission(env: &HttpEnv) {
    let zip2 = pkg::build_task_package_custom(
        "org.example.r5-adm-st",
        "1.0.0",
        1,
        false,
        None,
        false,
        &["kv"],
    );
    let up2 = env.upload_pkg(&zip2, None, "up-r5-st2").await;
    let (q_op, q_rev) = seed_staged_install_op(env, "org.example.r5-adm-st", &up2, 11, false);

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
        &format!("/api/v1/plugin-operations/{q_op}/resume"),
        Some(TOKEN),
        &[
            ("if-match", &format!("\"{q_rev}\"")),
            ("idempotency-key", "r5-res-ro"),
        ],
        Some(json!({})),
    );
    let mut fut = Box::pin(env.send(req));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut fut)
            .await
            .is_err(),
        "staged resume must wait on held Db::App writer"
    );
    let _ = env.state.clone().with_instance_writable(false);
    let _ = rel_tx.send(());
    let (st_ro, _, b_ro) = fut.await;
    let _ = env.state.clone().with_instance_writable(true);
    holder.join().unwrap();
    assert_err(st_ro, &b_ro, StatusCode::CONFLICT, "instance.read_only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r5_item1b_non_staged_resume_projects_job_and_cleanup_token_replay_succeeds() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let victim = stage.join("R5Replay.mkv");
    std::fs::write(&victim, b"payload").unwrap();
    let dl = Arc::new(MemoryDownloader::new(stage.clone()));
    let (env, site_id, dl_id) = env_with_dl_maps(dir, dl, vec![]);
    let store = env.state.store();

    let boost_rec = store.get_plugin("builtin.boost").unwrap().unwrap();
    let auth = store
        .issue_plugin_state_authority(&boost_rec, None)
        .unwrap();
    let cmd_auth = store
        .authorize_plugin_command(
            &auth,
            "downloader.control",
            &[PluginCommandTarget::Downloader(dl_id)],
        )
        .unwrap();
    let acc = store
        .accept_plugin_command(
            &cmd_auth,
            &AcceptPluginOperation {
                plugin_id: Some("builtin.boost".into()),
                subject: PluginSubject::User(admin_id()),
                idempotency_key: "r5-blocked-cmd".into(),
                request: dummy_req(20),
                kind: "downloader.control".into(),
                expected_revision: Some(boost_rec.revision),
                expected_generation: Some(boost_rec.generation),
                cause: PluginCommandCause::root("r5-test"),
            },
            br#"{"action":"resume"}"#,
        )
        .unwrap();
    let op_id = acc.operation.operation_id;
    let claim = store
        .claim_plugin_command(
            op_id,
            acc.operation.revision,
            Some(boost_rec.generation),
            0,
            1_700_000_010,
        )
        .unwrap();
    let blocked = store
        .record_plugin_command_outcome(
            &claim,
            &PluginCommandOutcome {
                state: PluginOperationState::Blocked,
                phase: "await-manual".into(),
                next_run_at: None,
                result: None,
                effect: PluginEffectObservation::NoEffect,
            },
        )
        .unwrap();

    let (st_res, _, b_res) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{}\"", blocked.revision)),
                ("idempotency-key", "r5-resume-nonstaged"),
            ],
            Some(json!({})),
        ))
        .await;
    let resumed = assert_ok(st_res, &b_res, StatusCode::ACCEPTED);
    assert_eq!(resumed["state"], "queued");
    let resumed_rev = u32::try_from(resumed["revision"].as_i64().unwrap()).unwrap();
    assert!(
        env.state
            .jobs()
            .acquire()
            .unwrap()
            .plugin_operation_receipt(
                PluginOperationId::from_uuid(op_id),
                jobs::JobKind::PluginCommand,
                resumed_rev,
            )
            .unwrap()
            .is_some(),
        "resume_operation on non-staged command must project PluginCommand job into jobs.db"
    );

    assert_cleanup_token_consumed_wal_replay(&env, site_id, dl_id, &stage, &victim).await;
}

async fn assert_cleanup_token_consumed_wal_replay(
    env: &HttpEnv,
    site_id: domain::SiteId,
    dl_id: domain::DownloaderId,
    stage: &std::path::Path,
    victim: &std::path::Path,
) {
    let store = env.state.store();
    let tf_id = seed_boost_task(
        store.as_ref(),
        dl_id,
        site_id,
        "r5-tok-replay",
        "1212121212121212121212121212121212121212",
        BoostTaskState::Seeding,
        7,
        0,
        Some(false),
        None,
    );
    let plan_json =
        json!({ "save_path": stage, "allowed_roots": [stage], "paths": [victim] }).to_string();
    let ret = store
        .mark_boost_task_retained_with_paths(&tf_id, 1, 7, Some(&plan_json), 1_700_000_010)
        .unwrap();
    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: tf_id,
            expected_revision: ret.revision,
            cleanup_plan_handle: "r5-plan-replay".into(),
            cleanup_plan_json: plan_json,
            ttl_secs: 600,
            now_utc: api::job_loop::unix_now(),
        })
        .unwrap();
    let (st_p, _, b_p) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle=r5-plan-replay",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let tok = assert_ok(st_p, &b_p, StatusCode::OK)["confirmation_token"]
        .as_str()
        .unwrap()
        .to_string();

    let rev = env.get_plugin_rev("builtin.boost").await;
    let idem = "r5-df-wal-replay";
    let cmd_body = json!({
        "kind": "downloader.delete-files",
        "payload": {
            "host_file_plan_handle": "r5-plan-replay",
            "revision": planned.revision,
            "confirmation_token": tok,
        }
    });
    let mk_req = || {
        req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(cmd_body.clone()),
        )
    };
    let _ = http_canon_req("POST", "/plugins/builtin.boost/commands", &cmd_body);
    let ((st1, _, b1), (st2, _, b2)) = tokio::join!(env.send(mk_req()), env.send(mk_req()));
    let op1 = assert_ok(st1, &b1, StatusCode::ACCEPTED);
    let op2 = assert_ok(st2, &b2, StatusCode::ACCEPTED);
    assert_eq!(op1["operation_id"], op2["operation_id"]);
}
