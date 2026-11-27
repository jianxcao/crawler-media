#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::http::StatusCode;
use plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, assert_paged, etag, location,
    pkg::{build_task_package, build_task_package_custom},
    req_json,
};
use serde_json::json;
use store::plugins::{
    AcceptPluginOperation, CanonicalPluginRequest, PluginCommandCause, PluginSubject,
};
use uuid::Uuid;

#[tokio::test]
async fn operations_get_400_and_404_routes() {
    let env = HttpEnv::new().await;
    let (st_bad, _, b_bad) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-operations/not-a-uuid",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    let missing = Uuid::new_v4();
    let (st_404, _, b_404) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{missing}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_404,
        &b_404,
        StatusCode::NOT_FOUND,
        "plugin.operation_missing",
    );
}

#[tokio::test]
async fn operations_get_etag_cancel_and_resume_routes() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.ops", "1.0.0", 1, false);
    let rev = env.install_pkg(&pkg, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let acc = env
        .state
        .store()
        .accept_plugin_operation(&AcceptPluginOperation {
            plugin_id: Some("org.example.ops".into()),
            subject: PluginSubject::User(admin_id()),
            idempotency_key: "planted-op".into(),
            request: CanonicalPluginRequest {
                method: "POST".into(),
                path: "/plugins/org.example.ops/commands".into(),
                body_digest: [7u8; 32],
            },
            kind: "disable".into(),
            expected_revision: Some(rev),
            expected_generation: None,
            cause: PluginCommandCause::root("planted"),
        })
        .unwrap();
    let op_id = acc.operation.operation_id;

    let (st_g, h_g, b_g) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{op_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let op_rev = assert_ok(st_g, &b_g, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();
    assert_eq!(etag(&h_g), format!("\"{op_rev}\""));

    let (st_c, h_c, b_c) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/cancel"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{op_rev}\"")),
                ("idempotency-key", "op-cancel-1"),
            ],
            Some(json!({ "reason": "abort" })),
        ))
        .await;
    let canceled = assert_ok(st_c, &b_c, StatusCode::ACCEPTED);
    assert_eq!(location(&h_c), format!("/api/v1/plugin-operations/{op_id}"));
    assert_eq!(canceled["state"], "cancelled");

    let rev_c = canceled["revision"].as_i64().unwrap();
    let (st_r, _, b_r) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-operations/{op_id}/resume"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_c}\"")),
                ("idempotency-key", "op-resume-1"),
            ],
            Some(json!({ "evidence_ref": "check-1" })),
        ))
        .await;
    assert_err(st_r, &b_r, StatusCode::CONFLICT, "plugin.revision_conflict");
}

#[tokio::test]
async fn tasks_list_update_and_interval_validation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.tskup", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.tskup",
        detail["revision"].as_i64().unwrap(),
        "en-tskup",
    )
    .await;

    let (st_lt, _, b_lt) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskup/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (tasks, next) = assert_paged(st_lt, &b_lt);
    assert_eq!(tasks.len(), 1);
    assert_eq!(next, None);
    let def_rev0 = tasks[0]["definition_revision"].as_i64().unwrap();

    let (st_bad, _, b_bad) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.tskup/tasks/sync/update",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev0}\"")),
                ("idempotency-key", "tsk-bad"),
            ],
            Some(json!({
                "enabled": true, "schedule": { "interval_secs": 10 }, "config": { "action": "pull" }
            })),
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    let (st_up, h_up, b_up) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.tskup/tasks/sync/update", Some(TOKEN), &[("if-match", &format!("\"{def_rev0}\"")), ("idempotency-key", "tsk-up-1")], Some(json!({
            "enabled": true, "schedule": { "interval_secs": 120 }, "config": { "action": "pull" }
        }))))
        .await;
    assert_ok(st_up, &b_up, StatusCode::ACCEPTED);
    assert!(location(&h_up).starts_with("/api/v1/plugin-operations/"));

    let (st_lt2, _, b_lt2) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskup/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (tasks2, _) = assert_paged(st_lt2, &b_lt2);
    assert!(tasks2[0]["definition_revision"].as_i64().unwrap() > def_rev0);
}

#[tokio::test]
async fn tasks_run_and_receipts_cursor_pagination() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.tskrun", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.tskrun",
        detail["revision"].as_i64().unwrap(),
        "en-tsk",
    )
    .await;

    let (st_lt0, _, b_lt0) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskrun/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev0 = assert_paged(st_lt0, &b_lt0).0[0]["definition_revision"]
        .as_i64()
        .unwrap();

    let (st_up, _, b_up) = env
        .send(req_json("POST", "/api/v1/plugins/org.example.tskrun/tasks/sync/update", Some(TOKEN), &[("if-match", &format!("\"{def_rev0}\"")), ("idempotency-key", "tsk-up-run")], Some(json!({
            "enabled": true, "schedule": { "interval_secs": 120 }, "config": { "action": "pull" }
        }))))
        .await;
    assert_ok(st_up, &b_up, StatusCode::ACCEPTED);
    let (st_lt, _, b_lt) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskrun/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev1 = assert_paged(st_lt, &b_lt).0[0]["definition_revision"]
        .as_i64()
        .unwrap();

    let (st_run, h_run, b_run) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.tskrun/tasks/sync/run",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev1}\"")),
                ("idempotency-key", "tsk-run-1"),
            ],
            Some(json!({ "input": { "action": "manual" } })),
        ))
        .await;
    let op_run = assert_ok(st_run, &b_run, StatusCode::ACCEPTED);
    assert!(location(&h_run).starts_with("/api/v1/plugin-operations/"));
    assert!(op_run["job_id"].as_str().is_some());

    let (st_rc, _, b_rc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskrun/tasks/sync/receipts?limit=10",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_paged(st_rc, &b_rc);
    let (st_rc_bad, _, b_rc_bad) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tskrun/tasks/sync/receipts?cursor=not-uuid",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_rc_bad,
        &b_rc_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}

#[tokio::test]
async fn commands_submit_supported_and_unsupported_422() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package_custom(
        "org.example.cmd",
        "1.0.0",
        1,
        false,
        None,
        false,
        &["command:schedule.set", "command:notify.send"],
    );
    let grants = json!([
        { "capability": "command:schedule.set", "scope": "Global", "sensitive": false },
        { "capability": "command:notify.send", "scope": "Global", "sensitive": false }
    ]);
    let detail = env.install_pkg(&pkg, None, grants).await;
    env.enable_plugin(
        "org.example.cmd",
        detail["revision"].as_i64().unwrap(),
        "en-cmd",
    )
    .await;

    let (st_g, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.cmd",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev = assert_ok(st_g, &b_g, StatusCode::OK)["revision"]
        .as_i64()
        .unwrap();

    // Ungranted command fails with 403 plugin.forbidden BEFORE supports_kind (Caveat #3).
    let (st_403, _, b_403) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cmd/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cmd-403"),
            ],
            Some(json!({ "kind": "downloader.delete-files", "payload": {} })),
        ))
        .await;
    assert_err(st_403, &b_403, StatusCode::FORBIDDEN, "plugin.forbidden");

    let (st_422, _, b_422) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cmd/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cmd-422"),
            ],
            Some(json!({ "kind": "notify.send", "payload": {} })),
        ))
        .await;
    assert_err(
        st_422,
        &b_422,
        StatusCode::UNPROCESSABLE_ENTITY,
        "plugin.unsupported_command",
    );

    let (st_ok, h_ok, b_ok) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.cmd/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cmd-set-1"),
            ],
            Some(json!({
                "kind": "schedule.set",
                "payload": { "task_id": "sync", "interval_secs": 180, "enabled": true },
                "targets": [{ "kind": "global" }],
                "evidence_ref": "admin-panel"
            })),
        ))
        .await;
    let op = assert_ok(st_ok, &b_ok, StatusCode::ACCEPTED);
    assert!(location(&h_ok).starts_with("/api/v1/plugin-operations/"));
    assert_eq!(op["state"], "queued");
}

#[tokio::test]
async fn audit_filtering_and_cursor_pagination() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.aud", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.aud",
        detail["revision"].as_i64().unwrap(),
        "aud-en",
    )
    .await;
    let (_, _, b_g) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.aud",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let rev1 = b_g["data"]["revision"].as_i64().unwrap();
    env.disable_plugin("org.example.aud", rev1, "aud-dis").await;

    let (st_a1, _, b_a1) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.aud/audit?limit=2&from=2020-01-01T00:00:00Z",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (items1, next1) = assert_paged(st_a1, &b_a1);
    assert_eq!(items1.len(), 2);
    let c1 = next1.unwrap();
    let (st_a2, _, b_a2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/org.example.aud/audit?limit=2&cursor={c1}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert!(!assert_paged(st_a2, &b_a2).0.is_empty());

    let (st_act, _, b_act) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.aud/audit?action=enable",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(assert_paged(st_act, &b_act).0.len(), 1);

    for bad_q in ["cursor=bad", "from=not-a-ts", "offset=5", "limit=201"] {
        let (st_bad, _, b_bad) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugins/org.example.aud/audit?{bad_q}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        assert_err(
            st_bad,
            &b_bad,
            StatusCode::BAD_REQUEST,
            "plugin.invalid_input",
        );
    }
}

#[tokio::test]
async fn metrics_endpoint_and_validation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.met", "1.0.0", 1, false);
    env.install_pkg(&pkg, None, json!([])).await;

    let (st_m, _, b_m) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.met/metrics?window_secs=60",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(assert_ok(st_m, &b_m, StatusCode::OK)["window_secs"], 60);

    let (st_mb, _, b_mb) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.met/metrics?window_secs=0",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_mb,
        &b_mb,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}

#[tokio::test]
async fn audit_remains_readable_after_plugin_uninstall() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.auduninst";
    let pkg = build_task_package(pid, "1.0.0", 1, false);
    let d0 = env.install_pkg(&pkg, None, json!([])).await;
    let rev = d0["revision"].as_i64().unwrap();
    let (st_u, _, b_u) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/uninstall"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "aud-un-1"),
            ],
            Some(json!({ "remove_private_data": true })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_u, &b_u, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );

    let (st_get, _, _) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_get, StatusCode::NOT_FOUND);

    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/audit?limit=50"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (items, _) = assert_paged(st_aud, &b_aud);
    assert!(
        items.iter().any(|e| e["action"] == "uninstall"),
        "expected uninstall audit event after uninstall: {items:?}"
    );

    let (st_miss, _, b_miss) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.never-existed/audit",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_miss, &b_miss, StatusCode::NOT_FOUND, "plugin.missing");
}
