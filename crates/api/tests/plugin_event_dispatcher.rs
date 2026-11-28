#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use plugin_api::{DownloadAfterEvent, HOOK_WORLD, SiteCheckinAfterEvent, TransferCompletedEvent};
use serde_json::{Value, json};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};

use plugin_http_support::{HttpEnv, TOKEN, assert_ok, assert_paged, pkg, req_json};

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

struct WebhookStub {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    fail_mode: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WebhookStub {
    fn start(initially_fail: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let fail_mode = Arc::new(AtomicBool::new(initially_fail));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_c = hits.clone();
        let fail_c = fail_mode.clone();
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let (code, body) = if fail_c.load(Ordering::SeqCst) {
                        (500, b"{\"ok\":false}".as_slice())
                    } else {
                        hits_c.fetch_add(1, Ordering::SeqCst);
                        (200, b"{\"ok\":true}".as_slice())
                    };
                    let head = format!(
                        "HTTP/1.1 {code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            addr,
            hits,
            fail_mode,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for WebhookStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

async fn install_notify_plugin(
    env: &HttpEnv,
    plugin_id: &str,
    hook_id: &str,
    permissions: &[&str],
    webhook_addr: Option<SocketAddr>,
    enable: bool,
) {
    let perms_toml = permissions
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let http_toml = webhook_addr
        .map(|addr| {
            format!(
                "\n[[http]]\norigin = \"http://{addr}\"\npath_prefix = \"/webhook\"\nmethods = [\"POST\"]\n"
            )
        })
        .unwrap_or_default();
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Notify {plugin_id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "M4.3 event dispatcher test"
config_schema = "config.schema.json"
state_schema = 1
{http_toml}
[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = [{perms_toml}]

[[components.hooks]]
id = "{hook_id}"
priority = 10
"#
    );
    let pkg_bytes = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ]);
    let grants: Vec<Value> = permissions
        .iter()
        .map(|cap| json!({ "capability": cap, "scope": "Global", "sensitive": false }))
        .collect();
    let inst = env.install_pkg(&pkg_bytes, None, json!(grants)).await;
    if enable {
        let rev = inst["revision"].as_i64().unwrap();
        env.enable_plugin(plugin_id, rev, &format!("en-{plugin_id}"))
            .await;
    }
}

#[tokio::test]
async fn dual_grant_gate_requires_hooks_and_events_and_enabled_lifecycle() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv_good = WebhookStub::start(false);
    let srv_no_event = WebhookStub::start(false);
    let srv_disabled = WebhookStub::start(false);

    install_notify_plugin(
        &env,
        "org.example.evtgood",
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv_good.addr),
        true,
    )
    .await;
    install_notify_plugin(
        &env,
        "org.example.evtnoevent",
        "download.after",
        &["hooks:download.after", "http"],
        Some(srv_no_event.addr),
        true,
    )
    .await;
    install_notify_plugin(
        &env,
        "org.example.evtdis",
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv_disabled.addr),
        false,
    )
    .await;

    let payload = serde_json::to_string(&DownloadAfterEvent {
        operation_id: "op-gate-1".into(),
        downloader_id: Some("dl-1".into()),
        info_hash: Some("hash-1".into()),
        outcome: "submitted".into(),
        verified_total_bytes: Some(2048),
    })
    .unwrap();
    let now_ms = store::unix_now_ms().unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::Subscribe,
            &EmitOutboxEventInput {
                event_id: "ev-dl-gate-1".into(),
                schema_version: 1,
                event_domain: "download".into(),
                event_kind: "download.after".into(),
                entity_type: "download_operation".into(),
                entity_id: "op-gate-1".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "cause-gate-1".into(),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"op-gate-1\"]".into(),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();

    api::drain_plugin_events_for_test(&env.state, now_ms).await;

    assert_eq!(srv_good.hits.load(Ordering::SeqCst), 1);
    assert_eq!(srv_no_event.hits.load(Ordering::SeqCst), 0);
    assert_eq!(srv_disabled.hits.load(Ordering::SeqCst), 0);

    let receipt = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-dl-gate-1", "org.example.evtgood")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, OutboxDeliveryStatus::Delivered);
    assert!(
        env.state
            .store()
            .get_delivery_receipt(
                OutboxDb::Subscribe,
                "ev-dl-gate-1",
                "org.example.evtnoevent"
            )
            .unwrap()
            .is_none()
    );

    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.evtgood/audit?action=download.after",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (aud_items, _) = assert_paged(st_aud, &b_aud);
    assert_eq!(aud_items.len(), 1);
    assert_eq!(aud_items[0]["state"], "succeeded");
}

#[tokio::test]
async fn wrong_context_in_notify_dead_letters_immediately_and_records_audit() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    install_notify_plugin(
        &env,
        "org.example.evtwc",
        "transfer.completed",
        &[
            "hooks:transfer.completed",
            "events:transfer.completed",
            "kv",
        ],
        None,
        true,
    )
    .await;

    let payload = serde_json::to_string(&TransferCompletedEvent {
        media_id: "m-1".into(),
        library_id: "lib-1".into(),
        relative_path: "Movie/Film.mkv".into(),
        file_count: 1,
        total_bytes: 4096,
    })
    .unwrap();
    let now_ms = store::unix_now_ms().unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::Library,
            &EmitOutboxEventInput {
                event_id: "ev-wc-1".into(),
                schema_version: 1,
                event_domain: "transfer".into(),
                event_kind: "transfer.completed".into(),
                entity_type: "media".into(),
                entity_id: "m-1".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "cause-wc-1".into(),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: Some("lib-1".into()),
                handles_json: "[\"m-1\"]".into(),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();

    api::drain_plugin_events_for_test(&env.state, now_ms).await;

    let (st_ev, _, b_ev) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.evtwc/events?status=dead_letter",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let ev_data = assert_ok(st_ev, &b_ev, StatusCode::OK);
    let items = ev_data["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["event_id"], "ev-wc-1");
    assert_eq!(items[0]["attempt_count"], 1);

    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.evtwc/audit?action=transfer.completed",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (aud_items, _) = assert_paged(st_aud, &b_aud);
    assert_eq!(aud_items.len(), 1);
    assert_eq!(aud_items[0]["state"], "rejected");
    assert!(aud_items[0]["phase"].as_str().unwrap().contains("wrong"));
}

#[tokio::test]
async fn poison_plugin_backoff_dead_letter_requeue_and_healthy_isolation() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv_poison = WebhookStub::start(true);
    let srv_healthy = WebhookStub::start(false);

    install_notify_plugin(
        &env,
        "org.example.evtpoison",
        "site.checkin.after",
        &[
            "hooks:site.checkin.after",
            "events:site.checkin.after",
            "http",
        ],
        Some(srv_poison.addr),
        true,
    )
    .await;
    install_notify_plugin(
        &env,
        "org.example.evthealthy",
        "site.checkin.after",
        &[
            "hooks:site.checkin.after",
            "events:site.checkin.after",
            "http",
        ],
        Some(srv_healthy.addr),
        true,
    )
    .await;

    let payload = serde_json::to_string(&SiteCheckinAfterEvent {
        site_id: "s-1".into(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-23".into(),
    })
    .unwrap();
    let t0 = store::unix_now_ms().unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: "ev-poison-1".into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: "s-1".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "cause-p-1".into(),
                subject: "system".into(),
                site_id: Some("s-1".into()),
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"s-1\"]".into(),
                payload_json: payload,
                occurred_at: t0,
                created_at: t0,
            },
        )
        .unwrap();

    // First drain delivers healthy immediately and schedules retry for poison.
    api::drain_plugin_events_for_test(&env.state, t0).await;
    assert_eq!(srv_healthy.hits.load(Ordering::SeqCst), 1);

    // Exhaust retry limit across advancing timestamps -> poison moves to dead_letter.
    for step in 1..=6i64 {
        api::drain_plugin_events_for_test(&env.state, t0 + step * 30_000).await;
    }

    let dl = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::App, "ev-poison-1", "org.example.evtpoison")
        .unwrap()
        .unwrap();
    assert_eq!(dl.status, OutboxDeliveryStatus::DeadLetter);

    // Fix webhook and requeue via admin HTTP endpoint -> delivers cleanly.
    srv_poison.fail_mode.store(false, Ordering::SeqCst);
    let (st_req, _, b_req) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.evtpoison/events/ev-poison-1/requeue",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_req, &b_req, StatusCode::OK);
    let after_req = store::unix_now_ms().unwrap();
    api::drain_plugin_events_for_test(&env.state, after_req).await;
    assert_eq!(srv_poison.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_recovery_reclaims_orphaned_delivering_rows_after_crash() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);
    install_notify_plugin(
        &env,
        "org.example.evtrec",
        "site.checkin.after",
        &[
            "hooks:site.checkin.after",
            "events:site.checkin.after",
            "http",
        ],
        Some(srv.addr),
        true,
    )
    .await;

    let payload = serde_json::to_string(&SiteCheckinAfterEvent {
        site_id: "s-rec".into(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-23".into(),
    })
    .unwrap();
    let t0 = store::unix_now_ms().unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: "ev-rec-1".into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: "s-rec".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "cause-rec-1".into(),
                subject: "system".into(),
                site_id: Some("s-rec".into()),
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"s-rec\"]".into(),
                payload_json: payload,
                occurred_at: t0,
                created_at: t0,
            },
        )
        .unwrap();

    // Seed + claim with long lease to simulate crashed worker mid-delivery.
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::App,
            &store::plugin_events::SeedSubscriberDeliveriesInput {
                event_id: "ev-rec-1".into(),
                plugin_ids: vec!["org.example.evtrec".into()],
                next_attempt_at: t0,
                now_ms: t0,
            },
        )
        .unwrap();
    env.state
        .store()
        .claim_due_deliveries(
            OutboxDb::App,
            &store::plugin_events::ClaimDueDeliveriesInput {
                plugin_id: Some("org.example.evtrec".into()),
                now_ms: t0,
                lease_duration_ms: 600_000,
                limit: Some(5),
            },
        )
        .unwrap();

    api::recover_plugin_events_startup_for_test(&env.state, t0 + 100);
    api::drain_plugin_events_for_test(&env.state, t0 + 200).await;
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);
}

fn emit_download_after(store: &api::Store, event_id: &str, op_id: &str, now_ms: i64) {
    let payload = serde_json::to_string(&DownloadAfterEvent {
        operation_id: op_id.into(),
        downloader_id: Some("dl-1".into()),
        info_hash: Some(format!("hash-{op_id}")),
        outcome: "submitted".into(),
        verified_total_bytes: Some(4096),
    })
    .unwrap();
    store
        .emit_outbox_event(
            OutboxDb::Subscribe,
            &EmitOutboxEventInput {
                event_id: event_id.into(),
                schema_version: 1,
                event_domain: "download".into(),
                event_kind: "download.after".into(),
                entity_type: "download_operation".into(),
                entity_id: op_id.into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{op_id}"),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: format!("[\"{op_id}\"]"),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}

fn emit_transfer_completed(store: &api::Store, event_id: &str, media_id: &str, now_ms: i64) {
    let payload = serde_json::to_string(&TransferCompletedEvent {
        media_id: media_id.into(),
        library_id: "lib-none".into(),
        relative_path: "NoSub/Film.mkv".into(),
        file_count: 1,
        total_bytes: 1024,
    })
    .unwrap();
    store
        .emit_outbox_event(
            OutboxDb::Library,
            &EmitOutboxEventInput {
                event_id: event_id.into(),
                schema_version: 1,
                event_domain: "transfer".into(),
                event_kind: "transfer.completed".into(),
                entity_type: "media".into(),
                entity_id: media_id.into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{event_id}"),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: Some("lib-none".into()),
                handles_json: format!("[\"{media_id}\"]"),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn fanout_list_plugins_read_error_retains_pending_event_and_delivers_after_recovery() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);
    install_notify_plugin(
        &env,
        "org.example.evtfault",
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv.addr),
        true,
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();
    emit_download_after(&env.state.store(), "ev-preseeded-1", "op-pre-1", t0);
    env.state
        .store()
        .seed_event_fanout(
            OutboxDb::Subscribe,
            &store::plugin_events::SeedSubscriberDeliveriesInput {
                event_id: "ev-preseeded-1".into(),
                plugin_ids: vec!["org.example.evtfault".into()],
                next_attempt_at: t0,
                now_ms: t0,
            },
        )
        .unwrap();
    emit_download_after(
        &env.state.store(),
        "ev-pending-fault-1",
        "op-fault-1",
        t0 + 1,
    );

    env.state
        .plugins()
        .set_fanout_read_fault_for_test(Some(Arc::new(|op| match op {
            api::FanoutReadOp::ListEnabledPlugins => Err(store::StoreError::ReadBusy),
            _ => Ok(()),
        })));
    api::drain_plugin_events_for_test(&env.state, t0 + 2).await;

    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);
    let pending = env
        .state
        .store()
        .list_pending_fanout_events(OutboxDb::Subscribe, None, Some(20))
        .unwrap();
    assert!(
        pending
            .items
            .iter()
            .any(|e| e.event_id == "ev-pending-fault-1"),
        "pending event must stay pending on transient subscriber list read error"
    );
    assert!(
        env.state
            .store()
            .get_delivery_receipt(
                OutboxDb::Subscribe,
                "ev-pending-fault-1",
                "org.example.evtfault"
            )
            .unwrap()
            .is_none()
    );

    env.state.plugins().set_fanout_read_fault_for_test(None);
    api::drain_plugin_events_for_test(&env.state, t0 + 10).await;
    api::drain_plugin_events_for_test(&env.state, t0 + 20).await;
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
    let receipt = env
        .state
        .store()
        .get_delivery_receipt(
            OutboxDb::Subscribe,
            "ev-pending-fault-1",
            "org.example.evtfault",
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(receipt.attempt_count, 1);

    emit_transfer_completed(&env.state.store(), "ev-nosub-1", "m-none", t0 + 30);
    api::drain_plugin_events_for_test(&env.state, t0 + 35).await;
    let lib_pending = env
        .state
        .store()
        .list_pending_fanout_events(OutboxDb::Library, None, Some(20))
        .unwrap();
    assert!(lib_pending.items.is_empty());
}

#[tokio::test]
async fn fanout_single_grant_read_error_avoids_partial_deliveries_and_recovers_after_restart() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv_a = WebhookStub::start(false);
    let srv_b = WebhookStub::start(false);
    install_notify_plugin(
        &env,
        "org.example.evta",
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv_a.addr),
        true,
    )
    .await;
    install_notify_plugin(
        &env,
        "org.example.evtb",
        "download.after",
        &["hooks:download.after", "events:download.after", "http"],
        Some(srv_b.addr),
        true,
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();
    emit_download_after(&env.state.store(), "ev-grant-fault-1", "op-grant-1", t0);

    // Fail grant lookup for the second plugin only -> neither plugin may be seeded yet.
    env.state
        .plugins()
        .set_fanout_read_fault_for_test(Some(Arc::new(|op| match op {
            api::FanoutReadOp::PluginGrants {
                plugin_id: "org.example.evtb",
            } => Err(store::StoreError::ReadBusy),
            _ => Ok(()),
        })));
    api::drain_plugin_events_for_test(&env.state, t0 + 5).await;

    let pending = env
        .state
        .store()
        .list_pending_fanout_events(OutboxDb::Subscribe, None, Some(20))
        .unwrap();
    assert!(
        pending
            .items
            .iter()
            .any(|e| e.event_id == "ev-grant-fault-1"),
        "event must remain pending when second plugin grant read fails"
    );
    assert!(
        env.state
            .store()
            .get_delivery_receipt(OutboxDb::Subscribe, "ev-grant-fault-1", "org.example.evta")
            .unwrap()
            .is_none(),
        "must not write partial delivery for first plugin when second plugin read failed"
    );
    assert_eq!(srv_a.hits.load(Ordering::SeqCst), 0);
    assert_eq!(srv_b.hits.load(Ordering::SeqCst), 0);

    // Simulate worker restart with a fresh ApiState on the same data directory.
    env.state.probe.shutdown_workers().await.unwrap();
    let restarted = plugin_http_support::build_state(env._dir.path());
    api::drain_plugin_events_for_test(&restarted, t0 + 20).await;
    assert_eq!(srv_a.hits.load(Ordering::SeqCst), 1);
    assert_eq!(srv_b.hits.load(Ordering::SeqCst), 1);
    for pid in ["org.example.evta", "org.example.evtb"] {
        let receipt = restarted
            .store()
            .get_delivery_receipt(OutboxDb::Subscribe, "ev-grant-fault-1", pid)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, OutboxDeliveryStatus::Delivered);
    }
    restarted.probe.shutdown_workers().await.unwrap();
}
