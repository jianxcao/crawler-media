use std::time::Duration;

use axum::http::StatusCode;
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginCommandOutcome,
    PluginEffectObservation, PluginOperationState, PluginSubject, StagedPluginIntent,
    StagedPluginOpKind,
};
use uuid::Uuid;

use crate::support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, pkg, req_json,
};

fn dummy_req(tag: u8) -> CanonicalPluginRequest {
    CanonicalPluginRequest {
        method: "POST".into(),
        path: "/plugin-operations/op/resume".into(),
        body_digest: [tag; 32],
    }
}

fn seed_queued_staged_install(env: &HttpEnv, pid: &str, up: &Value, tag: u8) -> (Uuid, i64) {
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
                idempotency_key: format!("r6-seed-st-{tag}"),
                request: dummy_req(tag),
                op_kind: StagedPluginOpKind::Install,
                expected_revision: 0,
                intent,
                cause: PluginCommandCause::root(format!("r6-seed:{tag}")),
            })?;
            Ok((acc.operation.operation_id, acc.operation.revision))
        })
        .unwrap()
}

async fn seed_draining_staged_upgrade(
    env: &HttpEnv,
    pid: &str,
    tag: u8,
) -> (Uuid, i64, Box<dyn Send>) {
    let zip_v1 = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let inst = env
        .install_pkg(
            &zip_v1,
            None,
            json!([{"capability": "kv", "scope": "Global", "sensitive": false}]),
        )
        .await;
    let en_op = env
        .enable_plugin(
            pid,
            inst["revision"].as_i64().unwrap(),
            &format!("en-{tag}"),
        )
        .await;
    assert_eq!(en_op["state"], "succeeded");

    let zip_v2 = pkg::build_task_package_custom(pid, "1.1.0", 1, false, None, false, &["kv"]);
    let up_v2 = env.upload_pkg(&zip_v2, None, &format!("up-v2-{tag}")).await;
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
        state_policy: None,
        target_config_digest: None,
        grants: vec![],
    };
    let (op_id, blocked_rev) = env
        .state
        .store()
        .app_write(|app| {
            let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                subject: PluginSubject::User(admin_id()),
                idempotency_key: format!("r6-upg-{tag}"),
                request: dummy_req(tag),
                op_kind: StagedPluginOpKind::Upgrade,
                expected_revision: cur.revision,
                intent,
                cause: PluginCommandCause::root(format!("r6-upg:{tag}")),
            })?;
            let op_id = acc.operation.operation_id;
            let claim =
                app.claim_staged_plugin_operation(op_id, acc.operation.revision, 0, 1_700_000_000)?;
            let p_after_begin = app.get_plugin(pid)?.unwrap();
            let _ = app.begin_plugin_lifecycle(pid, p_after_begin.revision, op_id, false)?;
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
    (op_id, blocked_rev, guard)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r6_item1a_staged_resume_preclaim_idempotency_and_duplicate_coordination() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let zip1 = pkg::build_task_package_custom(
        "org.example.r6-preclaim",
        "1.0.0",
        1,
        false,
        None,
        false,
        &["kv"],
    );
    let up1 = env.upload_pkg(&zip1, None, "up-r6-pre1").await;
    let (q_op, q_rev) = seed_queued_staged_install(&env, "org.example.r6-preclaim", &up1, 21);

    let store_bg = env.state.store();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let writer_thread = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let _ = ready_tx.send(());
                let _ = go_rx.recv();
                app.put_plugin_idempotency_envelope(
                    &PluginSubject::User(admin_id()),
                    "operation-resume",
                    "r6-idem-race",
                    &dummy_req(99),
                    &json!({"conflict": true}),
                )?;
                Ok(())
            })
            .unwrap();
    });
    ready_rx.recv().unwrap();

    let resume_fut = tokio::spawn({
        let app = env.app.clone();
        async move {
            use tower::ServiceExt;
            let resp = app
                .oneshot(req_json(
                    "POST",
                    &format!("/api/v1/plugin-operations/{q_op}/resume"),
                    Some(TOKEN),
                    &[
                        ("if-match", &format!("\"{q_rev}\"")),
                        ("idempotency-key", "r6-idem-race"),
                    ],
                    Some(json!({"evidence_ref": "real-req"})),
                ))
                .await
                .unwrap();
            let st = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (st, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    let _ = go_tx.send(());
    writer_thread.join().unwrap();
    let (st_race, b_race) = resume_fut.await.unwrap();
    assert_err(
        st_race,
        &b_race,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );
    let untouched = env
        .state
        .store()
        .get_plugin_operation_admin(q_op)
        .unwrap()
        .unwrap();
    assert_eq!(untouched.operation.state, PluginOperationState::Queued);
    assert_eq!(untouched.operation.revision, q_rev);

    assert_concurrent_resume_duplicates(&env).await;
}

async fn assert_concurrent_resume_duplicates(env: &HttpEnv) {
    let (u_op, u_rev, guard) =
        seed_draining_staged_upgrade(env, "org.example.r6-dup-upg", 22).await;

    let mk_resume = |idem: &str, body: Value| {
        req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{u_op}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{u_rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(body),
        )
    };
    let leader = {
        let app = env.app.clone();
        let req = mk_resume("r6-dup-inflight", json!({"evidence_ref": "same"}));
        tokio::spawn(async move {
            use tower::ServiceExt;
            let resp = app.oneshot(req).await.unwrap();
            let st = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (st, serde_json::from_slice::<Value>(&bytes).unwrap())
        })
    };
    for _ in 0..40 {
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

    let (st_diff, _, b_diff) = env
        .send(mk_resume(
            "r6-dup-inflight",
            json!({"evidence_ref": "different"}),
        ))
        .await;
    assert_err(
        st_diff,
        &b_diff,
        StatusCode::CONFLICT,
        "plugin.idempotency_conflict",
    );

    let follower = {
        let app = env.app.clone();
        let req = mk_resume("r6-dup-inflight", json!({"evidence_ref": "same"}));
        tokio::spawn(async move {
            use tower::ServiceExt;
            let resp = app.oneshot(req).await.unwrap();
            let st = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (st, serde_json::from_slice::<Value>(&bytes).unwrap())
        })
    };
    tokio::time::sleep(Duration::from_millis(40)).await;
    drop(guard);

    let (st1, b1) = leader.await.unwrap();
    let (st2, b2) = follower.await.unwrap();
    let v1 = assert_ok(st1, &b1, StatusCode::ACCEPTED);
    let v2 = assert_ok(st2, &b2, StatusCode::ACCEPTED);
    assert_eq!(v1, v2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r6_item1b_staged_resume_drain_over_1s_decoupled_from_store_write_wait() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let (u_op, u_rev, guard) =
        seed_draining_staged_upgrade(&env, "org.example.r6-slow-drain", 23).await;

    let store_bg = env.state.store();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_120));
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let contender = std::thread::spawn(move || {
            store_bg
                .app_write(|_| {
                    let _ = locked_tx.send(());
                    std::thread::sleep(Duration::from_millis(60));
                    Ok(())
                })
                .unwrap();
        });
        locked_rx.recv().unwrap();
        drop(guard);
        contender.join().unwrap();
    });

    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{u_op}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{u_rev}\"")),
                ("idempotency-key", "r6-slow-drain-key"),
            ],
            Some(json!({})),
        ))
        .await;
    releaser.join().unwrap();
    let view = assert_ok(st, &body, StatusCode::ACCEPTED);
    assert_eq!(view["state"], "failed");
    assert_eq!(view["phase"], "recovery-aborted");
}
