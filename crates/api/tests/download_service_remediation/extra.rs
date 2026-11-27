use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use api::{ApiDownloadService, DownloadOutcome, DownloadSubmitError, DownloadSubmitter};
use domain::{DownloaderId, PluginOperationId, SiteId};
use serde_json::json;
use store::plugins::{PluginCommandCause, PluginOperationState, PluginStateKind, PluginSubject};
use store::{DownloadOperationState, DownloadOwnershipState};

use super::lifecycle::FakeServer;
use super::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use super::support::*;
use std::io::Write;

async fn setup_dl_defer_kv_plugin(
    env: &HttpEnv,
    nil_dl: DownloaderId,
) -> (
    store::plugins::PluginRecord,
    store::plugins::PluginStateAuthority,
) {
    env.enable_dev_mode().await;
    let pkg = build_policy_pkg(
        "org.example.dl-defer-kv",
        "[[components.hooks]]\nid = \"download.before\"\npriority = 10\n",
        &["hooks:download.before", "command:downloader.add", "kv"],
    );
    let inst = env
        .install_pkg(
            &pkg,
            None,
            json!([
                { "capability": "hooks:download.before", "scope": "Global", "sensitive": false },
                { "capability": "kv", "scope": "Global", "sensitive": false },
                {
                    "capability": "command:downloader.add",
                    "scope": { "Object": { "kind": "downloader", "id": nil_dl.to_string() } },
                    "sensitive": false
                }
            ]),
        )
        .await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin("org.example.dl-defer-kv", rev, "en-kv")
        .await;
    let plugin = env
        .state
        .store()
        .get_plugin("org.example.dl-defer-kv")
        .unwrap()
        .unwrap();
    let auth = env
        .state
        .store()
        .issue_plugin_state_authority(&plugin, None)
        .unwrap();
    (plugin, auth)
}

#[tokio::test]
async fn plugin_downloader_add_retries_after_deferred_without_pk_collision() {
    let (bytes, _) = minimal_torrent_bytes("Plugin.Defer.Retry", 2048);
    let srv = StubHttpServer::start(bytes);
    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    let nil_dl = DownloaderId::from_uuid(uuid::Uuid::nil());
    let (plugin, auth) = setup_dl_defer_kv_plugin(&env, nil_dl).await;
    let entry = env
        .state
        .store()
        .plugin_state_put(&auth, PluginStateKind::Kv, "defer", b"1", 0)
        .unwrap();
    let payload = serde_json::to_vec(&json!({
        "downloader_id": nil_dl.to_string(),
        "download_ref": srv.url("cmd.torrent"),
        "paused": true
    }))
    .unwrap();
    let accepted = api::submit_plugin_command_for_test(
        &env.state,
        api::SubmitPluginCommand {
            role: api::PluginInvocationRole::Task,
            authority: auth.clone(),
            subject: PluginSubject::Task {
                plugin_id: plugin.plugin_id.clone(),
                task_id: "t1".into(),
            },
            kind: "downloader.add".into(),
            targets: vec![store::plugins::PluginCommandTarget::Downloader(nil_dl)],
            idempotency_key: "cmd-defer-retry-1".into(),
            request: store::plugins::CanonicalPluginRequest {
                method: "GUEST-CMD".into(),
                path: "/plugins/org.example.dl-defer-kv/commands/downloader.add".into(),
                body_digest: [1; 32],
            },
            expected_revision: None,
            cause: PluginCommandCause::root("cause-1"),
            payload,
        },
    )
    .unwrap();
    let cmd_op_id = PluginOperationId::from_uuid(accepted.operation.operation_id);

    api::run_plugin_command_for_test(&env.state, cmd_op_id).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin_operation_admin(cmd_op_id.as_uuid())
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Deferred
    );
    env.state
        .store()
        .plugin_state_delete(&auth, PluginStateKind::Kv, "defer", entry.revision)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    dl.unknown_once.store(true, Ordering::SeqCst);
    api::run_plugin_command_for_test(&env.state, cmd_op_id).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin_operation_admin(cmd_op_id.as_uuid())
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Unknown,
        "attempt 2 must not fail with UNIQUE constraint on download_operations"
    );

    api::reconcile_plugin_pending_for_test(&env.state).unwrap();
    api::run_plugin_reconcile_for_test(&env.state, cmd_op_id).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_plugin_operation_admin(cmd_op_id.as_uuid())
            .unwrap()
            .unwrap()
            .operation
            .state,
        PluginOperationState::Succeeded
    );
}

#[tokio::test]
async fn transmission_rpc_below_16_reconciles_unknown_and_repeat_owned() {
    let (bytes, hash) = minimal_torrent_bytes("Tx.Rpc15", 4096);
    let srv_t = StubHttpServer::start(bytes);
    let drop_add = Arc::new(AtomicBool::new(true));
    let added_hashes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (drop_c, hashes_c) = (Arc::clone(&drop_add), Arc::clone(&added_hashes));
    let fh_c = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let tx_srv = FakeServer::start(move |req, stream| {
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":15}}"#.to_string()
        } else if req.raw.contains("torrent-add") {
            hashes_c.lock().unwrap().push(hash.clone());
            if drop_c.load(Ordering::SeqCst) {
                return;
            }
            format!(
                r#"{{"result":"success","arguments":{{"torrent-added":{{"id":1,"hashString":"{hash}"}}}}}}"#
            )
        } else if req.raw.contains("torrent-get") {
            let mut rows: Vec<String> = hashes_c
                .lock()
                .unwrap()
                .iter()
                .map(|h| format!(r#"{{"id":1,"hashString":"{h}","error":0,"status":4}}"#))
                .collect();
            rows.push(format!(
                r#"{{"id":2,"hashString":"{fh_c}","error":0,"status":6}}"#
            ));
            format!(
                r#"{{"result":"success","arguments":{{"torrents":[{}]}}}}"#,
                rows.join(",")
            )
        } else {
            r#"{"result":"success","arguments":{}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: s1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    let env = make_env(
        Arc::new(RecordingDownloader::new(
            tempfile::tempdir().unwrap().keep(),
        )),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv_t.addr)),
    );
    let dl_id = DownloaderId::new();
    insert_tx_downloader(
        env.state.store().as_ref(),
        dl_id,
        &format!("{}/transmission/rpc", tx_srv.url()),
        false,
    );

    let svc = ApiDownloadService::new(env.state.clone());
    let t1 = test_torrent(SiteId::new(), None, "Tx.Rpc15", &srv_t.url("tx1.torrent"));
    let mut p1 = default_plan(t1, api::SubmissionPurpose::Manual);
    p1.downloader_id = Some(dl_id);
    let ctx1 = op_ctx("rpc15-unknown");
    let out1 = svc.submit(p1.clone(), ctx1.clone()).await.unwrap();
    assert!(matches!(out1, DownloadOutcome::Unknown { .. }));

    api::recover_plugin_startup_for_test(&env.state).unwrap();
    let op1 = env
        .state
        .store()
        .get_download_operation(&ctx1.operation_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(op1.state, DownloadOperationState::Submitted);
    let own = env
        .state
        .store()
        .list_download_ownership(
            None,
            Some(dl_id),
            Some(DownloadOwnershipState::Owned),
            None,
            Some(10),
        )
        .unwrap();
    assert_eq!(own.items.len(), 1);

    drop_add.store(false, Ordering::SeqCst);
    let out2 = svc.submit(p1, op_ctx("rpc15-repeat")).await.unwrap();
    assert!(matches!(
        out2,
        DownloadOutcome::Submitted {
            already_owned: true,
            ..
        }
    ));
}

#[tokio::test]
async fn transmission_rpc_below_16_rejects_genuine_foreign_duplicate() {
    let (f_bytes, f_hash) = minimal_torrent_bytes("Tx.Foreign", 1024);
    let srv_f = StubHttpServer::start(f_bytes);
    let env_f = make_env(
        Arc::new(RecordingDownloader::new(
            tempfile::tempdir().unwrap().keep(),
        )),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv_f.addr)),
    );
    let dl_f = DownloaderId::new();
    let tx_f = FakeServer::start(move |req, stream| {
        let body = if req.raw.contains("session-get") {
            r#"{"result":"success","arguments":{"rpc-version":15}}"#.to_string()
        } else if req.raw.contains("torrent-get") {
            format!(
                r#"{{"result":"success","arguments":{{"torrents":[{{"id":2,"hashString":"{f_hash}","error":0,"status":6}}]}}}}"#,
            )
        } else {
            r#"{"result":"success","arguments":{}}"#.to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: s1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    insert_tx_downloader(
        env_f.state.store().as_ref(),
        dl_f,
        &format!("{}/transmission/rpc", tx_f.url()),
        false,
    );
    let mut pf = default_plan(
        test_torrent(SiteId::new(), None, "Tx.Foreign", &srv_f.url("f.torrent")),
        api::SubmissionPurpose::Manual,
    );
    pf.downloader_id = Some(dl_f);
    let out_f = ApiDownloadService::new(env_f.state.clone())
        .submit(pf, op_ctx("rpc15-foreign"))
        .await
        .unwrap();
    assert!(
        matches!(out_f, DownloadOutcome::Rejected { ref reason, .. } if reason == "existing_foreign")
    );
}

#[tokio::test]
async fn submitting_crash_recovers_in_flight_guarded_and_resolve_source_error_fails_intent() {
    let (bytes, hash) = minimal_torrent_bytes("Submitting.Recover", 2048);
    let srv = StubHttpServer::start(bytes);
    let state_slot: Arc<Mutex<Option<api::ApiState>>> = Arc::new(Mutex::new(None));
    let slot_cb = Arc::clone(&state_slot);
    let dl = Arc::new(
        RecordingDownloader::new(tempfile::tempdir().unwrap().keep()).with_on_submit(Arc::new(
            move || {
                if let Some(st) = slot_cb.lock().unwrap().as_ref() {
                    api::reconcile_plugin_pending_for_test(st).unwrap();
                }
            },
        )),
    );
    let env = make_env(
        dl.clone(),
        Some(net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr)),
    );
    *state_slot.lock().unwrap() = Some(env.state.clone());

    let svc = ApiDownloadService::new(env.state.clone());
    let dl_id = DownloaderId::from_uuid(uuid::Uuid::nil());
    let crashed = env
        .state
        .store()
        .accept_download_operation(&store::AcceptDownloadOperationInput {
            operation_id: PluginOperationId::new().to_string(),
            idempotency_key: "crash-submitting-1".into(),
            source_kind: "manual".into(),
            subscribe_id: None,
            plugin_id: None,
            subject: "user:test".into(),
            cause_id: "c1".into(),
            site_id: None,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: srv.url("absent.torrent"),
            ownership_tag: "cm-op:crash-1".into(),
            info_hash: Some(hash),
            bytes_digest: None,
            verified_bytes: None,
            request_digest: "d1".into(),
            now_utc: 100,
        })
        .unwrap()
        .operation;
    env.state
        .store()
        .transition_download_operation(&store::TransitionDownloadOperationInput {
            operation_id: crashed.operation_id.clone(),
            expected_revision: crashed.revision,
            next_state: DownloadOperationState::Submitting,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 101,
        })
        .unwrap();
    api::recover_plugin_startup_for_test(&env.state).unwrap();
    assert_eq!(
        env.state
            .store()
            .get_download_operation(&crashed.operation_id)
            .unwrap()
            .unwrap()
            .state,
        DownloadOperationState::Failed
    );

    let out = svc
        .submit(
            default_plan(
                test_torrent(
                    SiteId::new(),
                    None,
                    "InFlight.Submit",
                    &srv.url("ok.torrent"),
                ),
                api::SubmissionPurpose::Manual,
            ),
            op_ctx("inflight-1"),
        )
        .await
        .unwrap();
    assert!(matches!(out, DownloadOutcome::Submitted { .. }));

    let mteam_id = SiteId::new();
    env.state
        .store()
        .insert_site(&test_site(mteam_id, "https://api.m-team.cc/", "mteam"))
        .unwrap();
    let bad_ctx = op_ctx("resolve-err-1");
    let err = svc
        .submit(
            default_plan(
                test_torrent(mteam_id, None, "Missing.MTeam.Id", &srv.url("mt.torrent")),
                api::SubmissionPurpose::Manual,
            ),
            bad_ctx.clone(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadSubmitError::InvalidPlan(_)));
    let failed_op = env
        .state
        .store()
        .get_download_operation(&bad_ctx.operation_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(failed_op.state, DownloadOperationState::Failed);
}

#[tokio::test]
async fn resolve_source_attaches_site_credentials_and_proxy_and_strips_on_redirect() {
    let (bytes, _) = minimal_torrent_bytes("Creds.Redirect", 2048);
    let srv_a = StubHttpServer::start(bytes.clone());
    let srv_b = StubHttpServer::start(bytes);
    *srv_a.redirect_to.lock().unwrap() = Some(srv_b.url("target.torrent"));

    let dl = Arc::new(RecordingDownloader::new(
        tempfile::tempdir().unwrap().keep(),
    ));
    let policy = net_guard::TargetPolicy::production()
        .with_test_loopback_socket(srv_a.addr)
        .with_test_loopback_socket(srv_b.addr);
    let env = make_env(dl, Some(policy));

    let site_id = SiteId::new();
    let mut site = test_site(site_id, &srv_a.url(""), "demo");
    site.cookie = Some("uid=42; pass=SECRET_CRED".into());
    site.api_key = Some("KEY_SECRET".into());
    env.state.store().insert_site(&site).unwrap();

    let svc = ApiDownloadService::new(env.state.clone());
    let out = svc
        .submit(
            default_plan(
                test_torrent(site_id, Some("1"), "Creds.Movie", &srv_a.url("dl.php?id=1")),
                api::SubmissionPurpose::Manual,
            ),
            op_ctx("creds-redir-1"),
        )
        .await
        .unwrap();
    assert!(matches!(out, DownloadOutcome::Submitted { .. }));

    let reqs_a = srv_a.captured_requests.lock().unwrap().clone();
    let reqs_b = srv_b.captured_requests.lock().unwrap().clone();
    assert!(!reqs_a.is_empty());
    assert!(!reqs_b.is_empty());
    let req_a_low = reqs_a[0].to_ascii_lowercase();
    let req_b_low = reqs_b[0].to_ascii_lowercase();
    assert!(
        req_a_low.contains("secret_cred"),
        "origin A must get Cookie: {}",
        reqs_a[0]
    );
    assert!(
        req_a_low.contains("key_secret"),
        "origin A must get X-Api-Key: {}",
        reqs_a[0]
    );
    assert!(
        !req_b_low.contains("secret_cred"),
        "redirect B must strip Cookie: {}",
        reqs_b[0]
    );
    assert!(
        !req_b_low.contains("key_secret"),
        "redirect B must strip X-Api-Key: {}",
        reqs_b[0]
    );

    let proxy_site = SiteId::new();
    let mut ps = test_site(proxy_site, &srv_a.url(""), "demo");
    ps.proxy = Some("http://127.0.0.1:8888".into());
    env.state.store().insert_site(&ps).unwrap();
    let err = svc
        .submit(
            default_plan(
                test_torrent(
                    proxy_site,
                    Some("2"),
                    "Proxy.Movie",
                    &srv_a.url("p.torrent"),
                ),
                api::SubmissionPurpose::Manual,
            ),
            op_ctx("creds-proxy-1"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadSubmitError::Fetch(_)));
    let _ = (TOKEN, assert_ok, req_json);
}
