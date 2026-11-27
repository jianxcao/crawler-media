use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::plugins::{
    AcceptStagedPluginOperation, CanonicalPluginRequest, PinPluginTrustKey, PluginCommandCause,
    PluginOperationState, PluginSubject, StagedPluginIntent, StagedPluginOpKind,
};

use crate::support::plugin_http_support::{HttpEnv, admin_id, assert_ok, pkg};
use crate::{post_prep, post_write};

fn http_canon_req(method: &str, path: &str, body: &Value) -> CanonicalPluginRequest {
    let bytes = serde_json::to_vec(body).unwrap();
    CanonicalPluginRequest {
        method: method.into(),
        path: path.into(),
        body_digest: Sha256::digest(&bytes).into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_d_r4_item5a_inflight_wal_commit_replays_after_token_consumption() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    // Part A: `create_trust_key` (`check_idempotent_replay_after_write`) while winner's
    // `tx.commit()` is still in flight on `Db::App` after consuming `reauth_token`!
    let key = pkg::TestSigningKey::generate(9501);
    let (_, b_p) = post_prep(
        &env,
        "/api/v1/plugin-trust-keys/prepare",
        json!({ "public_key": key.public_key_str, "label": "r4-wal-key" }),
    )
    .await;
    let prep = &b_p["data"];
    let reauth = env.reauth("trust-key:create").await;
    let idem = "r4-wal-tk-1";
    let body = json!({
        "consent_token": prep["consent_token"],
        "fingerprint": prep["fingerprint"],
        "label": "r4-wal-key",
        "public_key": prep["public_key"],
        "reauth_token": reauth,
    });
    let creq = http_canon_req("POST", "/plugin-trust-keys", &body);

    // Consume reauth token in memory just as winner does right before `tx.commit()`.
    env.state
        .plugins()
        .verify_and_consume_reauth_token(
            &env.state,
            admin_id(),
            api::plugins::ReauthPurpose::TrustKeyCreate,
            &reauth,
        )
        .await
        .unwrap();

    let kid = prep["key_id"].as_str().unwrap().to_string();
    let pk = prep["public_key"].as_str().unwrap().to_string();
    let fp = prep["fingerprint"].as_str().unwrap().to_string();
    let store_bg = store.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let created = app.pin_plugin_trust_key(&PinPluginTrustKey {
                    key_id: kid.clone(),
                    label: "r4-wal-key".into(),
                    public_key: pk,
                    fingerprint: fp,
                    actor: admin_id().to_string(),
                })?;
                let view = json!({
                    "key_id": created.key_id,
                    "label": created.label,
                    "public_key": created.public_key,
                    "fingerprint": created.fingerprint,
                    "status": "trusted",
                    "revision": created.revision,
                    "associated_package_count": 0,
                    "created_at": "2026-11-24T00:00:00Z",
                    "created_by": created.created_by,
                    "revoked_at": Value::Null,
                    "revoked_by": Value::Null,
                    "revoked_reason": Value::Null,
                });
                app.put_plugin_idempotency_envelope(
                    &PluginSubject::User(admin_id()),
                    "trust-key-create",
                    idem,
                    &creq,
                    &view,
                )?;
                let _ = entered_tx.send(());
                std::thread::sleep(Duration::from_millis(60));
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    let (st_tk, b_tk) = post_write(&env, "/api/v1/plugin-trust-keys", 0, idem, body).await;
    bg.join().unwrap();
    let replayed_tk = assert_ok(st_tk, &b_tk, StatusCode::CREATED);
    assert_eq!(replayed_tk["label"], "r4-wal-key");

    assert_inflight_wal_management_replay(&env).await;
}

async fn assert_inflight_wal_management_replay(env: &HttpEnv) {
    let store = env.state.store();
    let pid = "org.example.r4-wal-sr";
    let zip = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let rev = env.install_pkg(&zip, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();

    let (st_sp, b_sp) = post_prep(
        env,
        format!("/api/v1/plugins/{pid}/state/reset/prepare"),
        json!({ "security_scopes": [] }),
    )
    .await;
    let prep = assert_ok(st_sp, &b_sp, StatusCode::OK);
    let reauth = env.reauth("plugin:state-reset").await;
    let idem = "r4-wal-sr-1";
    let body = json!({
        "consent_token": prep["consent_token"],
        "reauth_token": reauth,
        "security_scopes": [],
    });
    let creq = http_canon_req("POST", &format!("/plugins/{pid}/state/reset"), &body);

    env.state
        .plugins()
        .verify_and_consume_reauth_token(
            &env.state,
            admin_id(),
            api::plugins::ReauthPurpose::PluginStateReset,
            &reauth,
        )
        .await
        .unwrap();

    let store_bg = store.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let acc = app.begin_staged_plugin_operation(&AcceptStagedPluginOperation {
                    subject: PluginSubject::User(admin_id()),
                    idempotency_key: idem.into(),
                    request: creq,
                    op_kind: StagedPluginOpKind::StateReset,
                    expected_revision: rev,
                    intent: StagedPluginIntent {
                        target_plugin_id: pid.into(),
                        package_hash: None,
                        version: None,
                        artifact_path: None,
                        state_policy: Some("reset".into()),
                        target_config_digest: None,
                        grants: Vec::new(),
                    },
                    cause: PluginCommandCause::root(format!("state-reset:{pid}")),
                })?;
                let op_id = acc.operation.operation_id;
                let claim = app.claim_staged_plugin_operation(
                    op_id,
                    acc.operation.revision,
                    0,
                    1_700_000_000,
                )?;
                let _ = app.reset_plugin_state(pid, rev, Some(op_id), None)?;
                app.complete_staged_plugin_operation(
                    &claim,
                    PluginOperationState::Succeeded,
                    "reset",
                    Some(
                        &json!({ "plugin_id": pid, "deleted_kv_rows": 0, "deleted_blob_rows": 0 }),
                    ),
                    None,
                    1_700_000_001,
                )?;
                let _ = entered_tx.send(op_id);
                std::thread::sleep(Duration::from_millis(60));
                Ok(())
            })
            .unwrap();
    });
    let expected_op = entered_rx.recv().unwrap();

    let (st_sr, b_sr) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/state/reset"),
        rev,
        idem,
        body,
    )
    .await;
    bg.join().unwrap();
    let replayed_sr = assert_ok(st_sr, &b_sr, StatusCode::ACCEPTED);
    assert_eq!(replayed_sr["operation_id"], expected_op.to_string());
}

async fn burst_posts(
    env: &Arc<HttpEnv>,
    path: String,
    rev: i64,
    idem: String,
    body: Value,
) -> Vec<(StatusCode, Value)> {
    let delays_us: [u64; 4] = [0, 50, 200, 600];
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
async fn slice_d_r4_item5b_concurrent_identical_retries_replay_across_token_gated_endpoints() {
    let env = Arc::new(HttpEnv::new().await);
    env.enable_dev_mode().await;

    for round in 0..3u64 {
        let key = pkg::TestSigningKey::generate(9600 + round);
        let (st_p, b_p) = post_prep(
            &env,
            "/api/v1/plugin-trust-keys/prepare",
            json!({ "public_key": key.public_key_str, "label": format!("r4-b-{round}") }),
        )
        .await;
        let prep = assert_ok(st_p, &b_p, StatusCode::OK);
        let reauth = env.reauth("trust-key:create").await;
        let replies = burst_posts(
            &env,
            "/api/v1/plugin-trust-keys".into(),
            0,
            format!("r4-tk-burst-{round}"),
            json!({
                "label": format!("r4-b-{round}"),
                "public_key": prep["public_key"],
                "fingerprint": prep["fingerprint"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            }),
        )
        .await;
        for (st, b) in replies {
            assert_eq!(
                assert_ok(st, &b, StatusCode::CREATED)["key_id"],
                prep["key_id"]
            );
        }
    }

    let pid = "org.example.r4-burst-perm";
    let zip = pkg::build_task_package_custom(pid, "1.0.0", 1, false, None, false, &["kv"]);
    let mut rev = env.install_pkg(&zip, None, json!([])).await["revision"]
        .as_i64()
        .unwrap();
    let kv_grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let (st_gp, b_gp) = post_prep(
        &env,
        format!("/api/v1/plugins/{pid}/permissions/prepare"),
        json!({ "requested_grants": kv_grants }),
    )
    .await;
    let prep_g = assert_ok(st_gp, &b_gp, StatusCode::OK);
    let g_replies = burst_posts(
        &env,
        format!("/api/v1/plugins/{pid}/permissions/grant"),
        rev,
        "r4-grant-burst".into(),
        json!({ "consent_token": prep_g["consent_token"], "grants": kv_grants }),
    )
    .await;
    let mut g_op: Option<String> = None;
    for (st, b) in g_replies {
        let cur = assert_ok(st, &b, StatusCode::ACCEPTED)["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(g_op.get_or_insert(cur.clone()), &cur);
    }

    rev = env.get_plugin_rev(pid).await;
    let (st_rp, b_rp) = post_prep(
        &env,
        format!("/api/v1/plugins/{pid}/state/reset/prepare"),
        json!({ "security_scopes": [] }),
    )
    .await;
    let prep_r = assert_ok(st_rp, &b_rp, StatusCode::OK);
    let reauth_r = env.reauth("plugin:state-reset").await;
    let r_replies = burst_posts(
        &env,
        format!("/api/v1/plugins/{pid}/state/reset"),
        rev,
        "r4-reset-burst".into(),
        json!({
            "consent_token": prep_r["consent_token"],
            "reauth_token": reauth_r,
            "security_scopes": [],
        }),
    )
    .await;
    let mut r_op: Option<String> = None;
    for (st, b) in r_replies {
        let cur = assert_ok(st, &b, StatusCode::ACCEPTED)["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(r_op.get_or_insert(cur.clone()), &cur);
    }
}
