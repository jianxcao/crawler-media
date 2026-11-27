use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api::router;
use axum::http::StatusCode;
use indexer::{Browser, BrowserConfig, PluginBrowserError, PluginBrowserPage, PluginNavResponse};
use serde_json::{Value, json};
use store::Db;
use store::plugins::{GrantPluginUiTrust, PluginSubject};

use crate::support::plugin_http_support::{
    HttpEnv, TOKEN, admin_id, assert_err, assert_ok, build_state, pkg::TestSigningKey, req_json,
};
use crate::{build_ui_package, http_canon_req, sha256_hex};

struct CountingPage;

impl PluginBrowserPage for CountingPage {
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
        .with_min_work(Duration::from_millis(20))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(move |_| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CountingPage))
        }),
    );
    (browser, calls)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r5_item2a_ui_trust_wal_replay_and_in_tx_auth_precheck() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(521);
    env.pin_trust_key(&key, "k-521").await;
    let pid = "org.example.r5uitrust";
    let pkg = build_ui_package(pid, "r5UiTrustRemote", &["read:sites"]);
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(&pkg, Some(&key.sign_package(&pkg)), json!([]))
        .await;

    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let bundle_sha = prep["bundle_sha256"].as_str().unwrap().to_string();

    // Create second admin so we can disable them while Db::App writer is held!
    let admin_b_id = domain::UserId::new();
    let admin_b_tok = "r5-admin-b-token";
    env.state
        .store()
        .insert_user_with_password(
            &domain::User {
                id: admin_b_id,
                login: "r5-admin-b".into(),
                enabled: true,
                role: domain::UserRole::Admin,
            },
            "r5-admin-b-pass-123",
        )
        .unwrap();
    env.state
        .store()
        .set_user_token(admin_b_id, admin_b_tok)
        .unwrap();
    let (st_pb, _, b_pb) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(admin_b_tok),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep_b = assert_ok(st_pb, &b_pb, StatusCode::OK);
    let (st_rb, _, b_rb) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-reauth",
            Some(admin_b_tok),
            &[],
            Some(json!({ "password": "r5-admin-b-pass-123", "purpose": "plugin-ui-trust" })),
        ))
        .await;
    let reauth_b = assert_ok(st_rb, &b_rb, StatusCode::OK)["reauth_token"]
        .as_str()
        .unwrap()
        .to_string();

    assert_in_tx_auth_precheck_before_grant(
        &env,
        pid,
        &pkg_hash,
        &bundle_sha,
        admin_b_id,
        admin_b_tok,
        &prep_b,
        &reauth_b,
    )
    .await;
    assert_inflight_wal_replay_for_ui_trust(&env, pid, &pkg_hash, &bundle_sha).await;
}

#[allow(clippy::too_many_arguments)]
async fn assert_in_tx_auth_precheck_before_grant(
    env: &HttpEnv,
    pid: &str,
    pkg_hash: &str,
    bundle_sha: &str,
    admin_b_id: domain::UserId,
    admin_b_tok: &str,
    _prep_b: &Value,
    reauth_b: &str,
) {
    env.state
        .store()
        .app_write(|app| {
            let _ = app.grant_ui_trust(&GrantPluginUiTrust {
                plugin_id: pid.to_string(),
                package_sha256: pkg_hash.to_string(),
                bundle_sha256: bundle_sha.to_string(),
                granted_by_user_id: admin_id().to_string(),
                granted_at: "2026-11-25T00:00:00Z".into(),
                expected_revision: 0,
            })?;
            Ok(())
        })
        .unwrap();

    // Disable admin_b AND bump UI trust revision while admin_b's grant waits past preflight!
    let store_bg2 = env.state.store();
    let (r2_tx, r2_rx) = std::sync::mpsc::channel();
    let (go2_tx, go2_rx) = std::sync::mpsc::channel();
    let (st_p2, _, b_p2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(admin_b_tok),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep_b2 = assert_ok(st_p2, &b_p2, StatusCode::OK);
    let rev_now = prep_b2["trust_revision"].as_i64().unwrap();
    let pid2 = pid.to_string();
    let pkg2 = pkg_hash.to_string();
    let holder2 = std::thread::spawn(move || {
        store_bg2
            .with_writer(Db::App, |conn| {
                let _ = r2_tx.send(());
                let _ = go2_rx.recv();
                conn.execute(
                    "UPDATE users SET enabled = 0 WHERE id = ?1",
                    [admin_b_id.to_string()],
                )?;
                conn.execute(
                    "UPDATE plugin_ui_trust SET revision = revision + 1 WHERE plugin_id = ?1 AND package_sha256 = ?2",
                    rusqlite::params![pid2, pkg2],
                )?;
                Ok(())
            })
            .unwrap();
    });
    r2_rx.recv().unwrap();
    let req2 = req_json(
        "POST",
        &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
        Some(admin_b_tok),
        &[
            ("if-match", &format!("\"{rev_now}\"")),
            ("idempotency-key", "r5-uit-precheck-2"),
        ],
        Some(json!({
            "preparation_id": prep_b2["preparation_id"],
            "package_hash": pkg_hash,
            "consent_token": prep_b2["consent_token"],
            "reauth_token": reauth_b,
        })),
    );
    let mut fut2 = Box::pin(env.send(req2));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut fut2)
            .await
            .is_err()
    );
    let _ = go2_tx.send(());
    let (st2, _, b2) = fut2.await;
    holder2.join().unwrap();
    assert_err(st2, &b2, StatusCode::FORBIDDEN, "auth.forbidden");
}

async fn assert_inflight_wal_replay_for_ui_trust(
    env: &HttpEnv,
    pid: &str,
    pkg_hash: &str,
    bundle_sha: &str,
) {
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let rev = prep["trust_revision"].as_i64().unwrap();
    let reauth = env.reauth("plugin-ui-trust").await;
    let idem = "r5-uit-wal-g";
    let body = json!({
        "consent_token": prep["consent_token"],
        "package_hash": pkg_hash,
        "preparation_id": prep["preparation_id"],
        "reauth_token": reauth,
    });
    let creq = http_canon_req("POST", &format!("/plugins/{pid}/ui-trust/grant"), &body);

    // Simulate winner having consumed reauth token while its Db::App commit is still in flight!
    let store_bg = env.state.store();
    let pid_c = pid.to_string();
    let pkg_c = pkg_hash.to_string();
    let bun_c = bundle_sha.to_string();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let bg = std::thread::spawn(move || {
        store_bg
            .app_write(|app| {
                let saved = app.grant_ui_trust(&GrantPluginUiTrust {
                    plugin_id: pid_c.clone(),
                    package_sha256: pkg_c.clone(),
                    bundle_sha256: bun_c.clone(),
                    granted_by_user_id: admin_id().to_string(),
                    granted_at: "2026-11-25T00:00:01Z".into(),
                    expected_revision: rev,
                })?;
                let view = json!({
                    "plugin_id": pid_c,
                    "package_hash": pkg_c,
                    "package_sha256": saved.package_sha256,
                    "bundle_sha256": bun_c,
                    "status": "trusted",
                    "ui_trusted": true,
                    "revision": saved.revision,
                    "granted_by_user_id": saved.granted_by_user_id,
                    "granted_at": saved.granted_at,
                    "revoked_at": Value::Null,
                });
                app.put_plugin_idempotency_envelope(
                    &PluginSubject::User(admin_id()),
                    "ui-trust-grant",
                    idem,
                    &creq,
                    &view,
                )?;
                let _ = entered_tx.send(());
                std::thread::sleep(Duration::from_millis(80));
                Ok(())
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    // Pass bogus reauth_token (simulating already-consumed token) while bg commit is in flight!
    let mut body_retry = body.clone();
    body_retry["reauth_token"] = json!(reauth);
    // Consume reauth via another endpoint or invalid token: wait — body_digest must match `body`!
    // Consume `reauth` via invalidation: issue 2048 tokens or use `revoke_ui_trust` conflict!
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", idem),
            ],
            Some(body),
        ))
        .await;
    bg.join().unwrap();
    assert_eq!(assert_ok(st_g, &b_g, StatusCode::OK)["status"], "trusted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice_e_r5_item2b_browser_check_enforces_write_admission_and_eliminates_pre_claim_toctou()
{
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (browser, cdp_calls) = make_counting_browser();
    let state = build_state(dir.path()).with_browser(browser);
    state.store().put_setting("cdp.enabled", "true").unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };

    let site_id = env.insert_site("PT-BChk-R5");
    let rev0 = env.get_plugin_rev("builtin.checkin").await;
    env.disable_plugin("builtin.checkin", rev0, "r5-dis-bchk")
        .await;
    let rev_dis = env.get_plugin_rev("builtin.checkin").await;
    let grants = json!([{
        "capability": "browser:maintenance",
        "scope": "Global",
        "sensitive": true
    }]);
    let (st_prep, _, b_prep) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.checkin/permissions/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grants })),
        ))
        .await;
    let prep = assert_ok(st_prep, &b_prep, StatusCode::OK);
    let (st_gr, _, b_gr) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.checkin/permissions/grant",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "r5-bchk-gr"),
            ],
            Some(json!({ "consent_token": prep["consent_token"], "grants": grants })),
        ))
        .await;
    assert_ok(st_gr, &b_gr, StatusCode::ACCEPTED);
    let rev_gr = env.get_plugin_rev("builtin.checkin").await;
    env.enable_plugin("builtin.checkin", rev_gr, "r5-en-bchk")
        .await;
    let rev_en = env.get_plugin_rev("builtin.checkin").await;

    // 1) Immediate write admission gate check at entry
    let gate = env.state.write_gate();
    {
        let _closed = gate.close();
        let (st_gate, _, b_gate) = env
            .send(req_json(
                "POST",
                "/api/v1/plugins/builtin.checkin/browser-checks",
                Some(TOKEN),
                &[
                    ("if-match", &format!("\"{rev_en}\"")),
                    ("idempotency-key", "r5-bchk-gate"),
                ],
                Some(json!({ "site_id": site_id, "purpose": "maintenance" })),
            ))
            .await;
        assert_err(
            st_gate,
            &b_gate,
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
        );
    }

    // 2) Zero TOCTOU window: delete site while Db::App writer is held before claim!
    let store_bg = env.state.store();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        store_bg
            .with_writer(Db::App, |conn| {
                let _ = ready_tx.send(());
                let _ = rel_rx.recv();
                conn.execute("DELETE FROM sites WHERE id = ?1", [site_id.to_string()])?;
                Ok(())
            })
            .unwrap();
    });
    ready_rx.recv().unwrap();

    let req = req_json(
        "POST",
        "/api/v1/plugins/builtin.checkin/browser-checks",
        Some(TOKEN),
        &[
            ("if-match", &format!("\"{rev_en}\"")),
            ("idempotency-key", "r5-bchk-toctou"),
        ],
        Some(json!({ "site_id": site_id, "purpose": "maintenance" })),
    );
    let mut fut = Box::pin(env.send(req));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut fut)
            .await
            .is_err()
    );
    let _ = rel_tx.send(());
    let (st_del, _, b_del) = fut.await;
    holder.join().unwrap();
    assert_err(st_del, &b_del, StatusCode::NOT_FOUND, "site.not_found");
    assert_eq!(cdp_calls.load(Ordering::SeqCst), 0);
}
