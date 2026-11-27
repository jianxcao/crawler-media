#[path = "builtin_boost/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use plugin_api::{DownloadAfterEvent, EndpointRequest, GuestRole, HOOK_WORLD, Header};
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};
use store::plugins::{PluginCommandCause, PluginSubject};
use support::plugin_http_support::{HttpEnv, TOKEN, admin_id, assert_ok, pkg, req_json};

const EP_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");
const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

async fn send_write(
    env: &HttpEnv,
    method: &str,
    path: &str,
    rev: i64,
    idem: &str,
    body: Value,
) -> (StatusCode, Value) {
    let r = format!("\"{rev}\"");
    let req = req_json(
        method,
        path,
        Some(TOKEN),
        &[("if-match", &r), ("idempotency-key", idem)],
        Some(body),
    );
    let (st, _, b) = env.send(req).await;
    (st, b)
}

fn build_ep_zip(id: &str, extra_caps: &[&str], custom_schema: Option<&str>) -> Vec<u8> {
    let mut perms = vec!["\"kv\"".to_string()];
    for cap in extra_caps {
        perms.push(format!("\"{cap}\""));
    }
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R7 Slice D {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "R7 Slice D test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "ep-main"
entry = "components/ep.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = [{}]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#,
        perms.join(", ")
    );
    let default_cfg = pkg::sample_schema(false, None);
    let cfg_schema = custom_schema.unwrap_or(&default_cfg);
    let ep_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/ep.schema.json", ep_schema.as_bytes()),
        ("components/ep.wasm", EP_WASM),
    ])
}

#[tokio::test]
async fn uninstall_and_reinstall_same_plugin_id_does_not_brick_runtime_dispatch() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r7-reinstall";
    let zip = build_ep_zip(pid, &[], None);
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);

    let inst1 = env.install_pkg(&zip, None, grants.clone()).await;
    env.enable_plugin(pid, inst1["revision"].as_i64().unwrap(), "r7-ri-en1")
        .await;

    let cur1 = env.state.store().get_plugin(pid).unwrap().unwrap();
    let auth1 = env
        .state
        .store()
        .issue_plugin_state_authority(&cur1, None)
        .unwrap();
    let resp1 = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &cur1,
            auth1,
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r7-ri-ep1"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"v1".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(resp1.status, 200);

    let rev_dis = env.get_plugin_rev(pid).await;
    let (st_dis, b_dis) = send_write(
        &env,
        "POST",
        &format!("/api/v1/plugins/{pid}/disable"),
        rev_dis,
        "r7-ri-dis1",
        json!({}),
    )
    .await;
    assert_ok(st_dis, &b_dis, StatusCode::ACCEPTED);

    let rev_un = env.get_plugin_rev(pid).await;
    let (st_un, b_un) = send_write(
        &env,
        "POST",
        &format!("/api/v1/plugins/{pid}/uninstall"),
        rev_un,
        "r7-ri-un1",
        json!({ "remove_private_data": true }),
    )
    .await;
    assert_ok(st_un, &b_un, StatusCode::ACCEPTED);
    assert!(env.state.store().get_plugin(pid).unwrap().is_none());

    let inst2 = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst2["revision"].as_i64().unwrap(), "r7-ri-en2")
        .await;

    let cur2 = env.state.store().get_plugin(pid).unwrap().unwrap();
    assert!(cur2.generation >= 2);
    let auth2 = env
        .state
        .store()
        .issue_plugin_state_authority(&cur2, None)
        .unwrap();
    let resp2 = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &cur2,
            auth2,
            PluginSubject::User(admin_id()),
            PluginCommandCause::root("r7-ri-ep2"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"v2".to_vec(),
            },
        )
        .expect("reinstalled plugin must dispatch cleanly without StaleGeneration");
    assert_eq!(resp2.status, 200);
}

struct WebhookStub {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    fail_on_hit: Arc<AtomicUsize>,
    fail_status: Arc<AtomicU16>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WebhookStub {
    fn start(fail_on_hit: usize, fail_status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let fail_at = Arc::new(AtomicUsize::new(fail_on_hit));
        let status_at = Arc::new(AtomicU16::new(fail_status));
        let stop = Arc::new(AtomicBool::new(false));
        let (hits_c, fail_c, status_c, stop_c) = (
            hits.clone(),
            fail_at.clone(),
            status_at.clone(),
            stop.clone(),
        );
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf);
                    let hit_num = hits_c.fetch_add(1, Ordering::SeqCst) + 1;
                    let (code, body) = if hit_num == fail_c.load(Ordering::SeqCst) {
                        (
                            status_c.load(Ordering::SeqCst),
                            b"{\"ok\":false}".as_slice(),
                        )
                    } else {
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
            fail_on_hit: fail_at,
            fail_status: status_at,
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

fn emit_download_event(env: &HttpEnv, event_id: &str, now_ms: i64) {
    let payload = serde_json::to_string(&DownloadAfterEvent {
        operation_id: format!("op-{event_id}"),
        downloader_id: Some("dl-1".into()),
        info_hash: Some("hash-1".into()),
        outcome: "submitted".into(),
        verified_total_bytes: Some(1024),
    })
    .unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::Subscribe,
            &EmitOutboxEventInput {
                event_id: event_id.into(),
                schema_version: 1,
                event_domain: "download".into(),
                event_kind: "download.after".into(),
                entity_type: "download_operation".into(),
                entity_id: format!("op-{event_id}"),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{event_id}"),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"h1\"]".into(),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}

async fn install_two_comp_notify_plugin(
    env: &HttpEnv,
    pid: &str,
    addr: SocketAddr,
    c1: (&str, i32),
    c2: (&str, i32),
    idem: &str,
) {
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R7 Multi Event {pid}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Multi-component notify checkpointing"
config_schema = "config.schema.json"
state_schema = 1

[[http]]
origin = "http://{addr}"
path_prefix = "/webhook"
methods = ["POST"]

[[components]]
id = "{}"
entry = "c1.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:download.after", "events:download.after", "http"]
[[components.hooks]]
id = "download.after"
priority = {}

[[components]]
id = "{}"
entry = "c2.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:download.after", "events:download.after", "http"]
[[components.hooks]]
id = "download.after"
priority = {}
"#,
        c1.0, c1.1, c2.0, c2.1
    );
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("c1.wasm", POLICY_WASM),
        ("c2.wasm", POLICY_WASM),
    ]);
    let grants = json!([
        { "capability": "hooks:download.after", "scope": "Global", "sensitive": false },
        { "capability": "events:download.after", "scope": "Global", "sensitive": false },
        { "capability": "http", "scope": "Global", "sensitive": false }
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), idem)
        .await;
}

#[tokio::test]
async fn multicomp_outbox_delivery_does_not_duplicate_succeeded_webhooks_or_starve_healthy_siblings()
 {
    // stub_retry fails hit #2 with 500 (retryable); stub_starve fails hit #1 with 400 (permanent).
    let stub_retry = WebhookStub::start(2, 500);
    let stub_starve = WebhookStub::start(1, 400);
    let mut env = HttpEnv::new().await;
    env.state = env.state.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production()
            .with_test_loopback_socket(stub_retry.addr)
            .with_test_loopback_socket(stub_starve.addr),
    );
    env.app = api::router(env.state.clone());
    env.enable_dev_mode().await;

    let pid_retry = "org.example.r7-evt-dedup";
    install_two_comp_notify_plugin(
        &env,
        pid_retry,
        stub_retry.addr,
        ("comp-1", 10),
        ("comp-2", 20),
        "r7-ed-en1",
    )
    .await;

    let pid_starve = "org.example.r7-evt-nostarve";
    install_two_comp_notify_plugin(
        &env,
        pid_starve,
        stub_starve.addr,
        ("comp-perm-fail", 5),
        ("comp-healthy", 15),
        "r7-ed-en2",
    )
    .await;

    let t0 = store::unix_now_ms().unwrap();
    emit_download_event(&env, "ev-r7-multi", t0);
    api::drain_plugin_events_for_test(&env.state, t0).await;

    let r_starve = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r7-multi", pid_starve)
        .unwrap()
        .unwrap();
    assert_eq!(r_starve.status, OutboxDeliveryStatus::DeadLetter);
    assert_eq!(
        stub_starve.hits.load(Ordering::SeqCst),
        2,
        "comp-perm-fail (400) AND comp-healthy (200) must both run on Attempt 1"
    );
    assert_eq!(
        stub_retry.hits.load(Ordering::SeqCst),
        2,
        "comp-1 (200) and comp-2 (500) must both execute on Attempt 1"
    );

    // Attempt 2 for pid_retry: comp-1 MUST NOT run again; only comp-2 runs (hit 3) -> Delivered!
    stub_retry.fail_on_hit.store(0, Ordering::SeqCst);
    api::drain_plugin_events_for_test(&env.state, t0 + 5_000).await;
    let r_retry = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r7-multi", pid_retry)
        .unwrap()
        .unwrap();
    assert_eq!(r_retry.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(
        stub_retry.hits.load(Ordering::SeqCst),
        3,
        "already-succeeded comp-1 must NOT re-send webhook on retry"
    );

    // Requeue dead-lettered pid_starve: comp-healthy MUST NOT run again; only comp-perm-fail runs!
    stub_starve.fail_on_hit.store(0, Ordering::SeqCst);
    stub_starve.fail_status.store(200, Ordering::SeqCst);
    let requeued = env
        .state
        .store()
        .requeue_dead_letter(
            OutboxDb::Subscribe,
            "ev-r7-multi",
            pid_starve,
            t0 + 6_000,
            t0 + 6_000,
        )
        .unwrap();
    assert_eq!(requeued.status, OutboxDeliveryStatus::Pending);
    api::drain_plugin_events_for_test(&env.state, t0 + 6_000).await;
    let r_starve2 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r7-multi", pid_starve)
        .unwrap()
        .unwrap();
    assert_eq!(r_starve2.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(
        stub_starve.hits.load(Ordering::SeqCst),
        3,
        "requeued dead-letter must NOT re-run already-succeeded comp-healthy"
    );
}

#[tokio::test]
async fn object_site_grant_authorizes_command_indexer_search_and_rss() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R7-IdxCmdSite");

    let pid = "org.example.r7-idx-cmd";
    let zip = build_ep_zip(
        pid,
        &["command:indexer.search", "command:indexer.rss"],
        None,
    );
    let grants = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "command:indexer.search", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false },
        { "capability": "command:indexer.rss", "scope": { "Object": { "kind": "site", "id": site_id.to_string() } }, "sensitive": false }
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r7-ic-en")
        .await;

    let cur = env.state.store().get_plugin(pid).unwrap().unwrap();
    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = u64::try_from(cur.generation).unwrap();
    inv.role = GuestRole::Endpoint;
    inv.provider_id = Some("ep-main".into());
    inv.site_id = Some(site_id.to_string());
    let _guard = plugin_host::HostPorts::begin_invocation(ports.as_ref(), &inv).unwrap();

    assert!(
        plugin_host::HostPorts::authorized(ports.as_ref(), &inv, "command:indexer.search"),
        "Object(site) grant must authorize command:indexer.search"
    );
    assert!(
        plugin_host::HostPorts::authorized(ports.as_ref(), &inv, "command:indexer.rss"),
        "Object(site) grant must authorize command:indexer.rss"
    );
}

#[tokio::test]
async fn config_validation_accepts_empty_optional_site_id_and_trimmed_sites_array() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R7-CfgSite");
    support::plugin_http_support::plugins_fixture::install_cm_checkin(
        &env, false, false, false, false,
    )
    .await;

    let (st_chk, _, checkin_cfg) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.checkin/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_chk, &checkin_cfg, StatusCode::OK);
    let checkin_srev = checkin_cfg["data"]["schema_revision"].as_str().unwrap();
    let (st_val, _, b_val) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/config/validate",
            Some(TOKEN),
            &[],
            Some(json!({
                "schema_revision": checkin_srev,
                "values": { "interval_secs": 86400, "site_id": "" }
            })),
        ))
        .await;
    assert_ok(st_val, &b_val, StatusCode::OK);

    let pid = "org.example.r7-cfg-sites";
    let custom_schema = r#"{
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "site_id": { "type": "string" },
            "sites": { "type": "array", "items": { "type": "string" } }
        }
    }"#;
    let zip = build_ep_zip(pid, &[], Some(custom_schema));
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let inst = env.install_pkg(&zip, None, grants).await;
    let rev = inst["revision"].as_i64().unwrap();
    let (st_pkg, _, pkg_cfg) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/config"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_pkg, &pkg_cfg, StatusCode::OK);
    let pkg_srev = pkg_cfg["data"]["schema_revision"].as_str().unwrap();

    let (st_put, b_put) = send_write(
        &env,
        "PUT",
        &format!("/api/v1/plugins/{pid}/config"),
        rev,
        "r7-cfg-put",
        json!({
            "schema_revision": pkg_srev,
            "values": {
                "site_id": "   ",
                "sites": [format!("  {site_id}  "), format!("site:{site_id}")]
            }
        }),
    )
    .await;
    assert_ok(st_put, &b_put, StatusCode::OK);
}
