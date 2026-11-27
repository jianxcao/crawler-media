#[path = "builtin_boost/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use plugin_api::{DownloadAfterEvent, HOOK_WORLD};
use plugin_host::Invocation;
use serde_json::{Value, json};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};
use support::plugin_http_support::{HttpEnv, TOKEN, assert_ok, pkg, req_json};

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

fn build_empty_cfg_ep_zip(id: &str, caps: &[&str]) -> Vec<u8> {
    let perms: Vec<String> = caps.iter().map(|c| format!("\"{c}\"")).collect();
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "R8 Slice D {id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "R8 Slice D"
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
    let empty_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", empty_schema.as_bytes()),
        ("schemas/ep.schema.json", empty_schema.as_bytes()),
        ("components/ep.wasm", EP_WASM),
    ])
}

#[tokio::test]
async fn pre_uninstall_generation_remains_fenced_after_uninstall_and_reinstall() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let pid = "org.example.r8-d-fence";
    let zip = build_empty_cfg_ep_zip(pid, &["kv"]);
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let inst1 = env.install_pkg(&zip, None, grants.clone()).await;
    env.enable_plugin(pid, inst1["revision"].as_i64().unwrap(), "r8-df-en1")
        .await;
    let stale_gen = env
        .state
        .store()
        .get_plugin(pid)
        .unwrap()
        .unwrap()
        .generation;

    let rev_dis = env.get_plugin_rev(pid).await;
    let (st_dis, b_dis) = send_write(
        &env,
        "POST",
        &format!("/api/v1/plugins/{pid}/disable"),
        rev_dis,
        "r8-df-dis1",
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
        "r8-df-un1",
        json!({ "remove_private_data": true }),
    )
    .await;
    assert_ok(st_un, &b_un, StatusCode::ACCEPTED);

    let inv_stale = Invocation::new(pid, Duration::from_secs(5));
    assert!(
        env.state
            .plugins()
            .enter_library_invocation_for_test(pid, stale_gen, &inv_stale)
            .is_err(),
        "pre-uninstall generation {stale_gen} must remain fenced after uninstall"
    );

    let inst2 = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst2["revision"].as_i64().unwrap(), "r8-df-en2")
        .await;
    let cur2 = env.state.store().get_plugin(pid).unwrap().unwrap();
    assert!(
        env.state
            .plugins()
            .enter_library_invocation_for_test(pid, stale_gen, &inv_stale)
            .is_err(),
        "pre-uninstall generation {stale_gen} must remain fenced after reinstall"
    );
    let _ok_guard = env
        .state
        .plugins()
        .enter_library_invocation_for_test(
            pid,
            cur2.generation,
            &Invocation::new(pid, Duration::from_secs(5)),
        )
        .expect("reinstalled new generation must enter cleanly");
}

#[tokio::test]
async fn rss_site_grant_covers_download_site() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R8-D-Site");
    let dl_id = env.insert_downloader("R8-D-DL");

    let pid_rss = "org.example.r8-d-rss-dl";
    let zip_rss = build_empty_cfg_ep_zip(
        pid_rss,
        &["kv", "command:indexer.rss", "command:downloader.add"],
    );
    let grants_rss = json!([
        { "capability": "kv", "scope": "Global", "sensitive": false },
        { "capability": "command:indexer.rss", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "command:downloader.add", "scope": "Global", "sensitive": false }
    ]);
    let inst_rss = env.install_pkg(&zip_rss, None, grants_rss).await;
    env.enable_plugin(pid_rss, inst_rss["revision"].as_i64().unwrap(), "r8-rss-en")
        .await;

    let rev_rss = env.get_plugin_rev(pid_rss).await;
    let (st_cmd, b_cmd) = send_write(
        &env,
        "POST",
        &format!("/api/v1/plugins/{pid_rss}/commands"),
        rev_rss,
        "r8-rss-dl-cmd",
        json!({
            "kind": "downloader.add",
            "payload": {
                "site_id": site_id,
                "downloader_id": dl_id,
                "download_ref": "https://pt.example/t/1.torrent",
                "title": "R8.RSS.Site.Test.1080p"
            }
        }),
    )
    .await;
    assert_eq!(
        st_cmd,
        StatusCode::ACCEPTED,
        "rss site grant must cover download site: {b_cmd}"
    );
}

#[tokio::test]
async fn task_with_empty_site_id_runs_cleanly() {
    let env = HttpEnv::new().await;
    let site_a = env.insert_site("R8-D-Site-A");
    let _site_b = env.insert_site("R8-D-Site-B");

    let (_, _, tasks_resp) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/builtin.checkin/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev = tasks_resp["data"][0]["definition_revision"]
        .as_i64()
        .or_else(|| tasks_resp["data"]["items"][0]["definition_revision"].as_i64())
        .unwrap();
    let (st_up, b_up) = send_write(
        &env,
        "POST",
        "/api/v1/plugins/builtin.checkin/tasks/check-in/update",
        def_rev,
        "r8-chk-up",
        json!({ "enabled": true, "schedule": { "interval_secs": 3600 }, "config": { "site_id": "" } }),
    )
    .await;
    assert_ok(st_up, &b_up, StatusCode::ACCEPTED);

    let (st_run, b_run) = send_write(
        &env,
        "POST",
        "/api/v1/plugins/builtin.checkin/tasks/check-in/run",
        def_rev + 1,
        "r8-chk-empty-site",
        json!({ "input": { "site_id": "   " } }),
    )
    .await;
    let run_view = assert_ok(st_run, &b_run, StatusCode::ACCEPTED);
    let job_id: domain::JobId = run_view["job_id"]
        .as_str()
        .or_else(|| run_view["result"]["job_id"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let job = env
        .state
        .jobs()
        .acquire()
        .unwrap()
        .get(job_id)
        .unwrap()
        .unwrap();
    let err_msg = api::plugins::run_task_job(&env.state, &job)
        .expect_err("must execute checkin across enabled sites, not silently skip");
    assert!(
        err_msg.contains("R8-D-Site-A") && err_msg.contains("R8-D-Site-B"),
        "empty site_id must run all enabled sites: {err_msg}"
    );

    let (st_up2, b_up2) = send_write(
        &env,
        "POST",
        "/api/v1/plugins/builtin.checkin/tasks/check-in/update",
        def_rev + 1,
        "r8-chk-up2",
        json!({ "enabled": true, "schedule": { "interval_secs": 3600 }, "config": { "site_id": site_a } }),
    )
    .await;
    assert_ok(st_up2, &b_up2, StatusCode::ACCEPTED);
    let (st_run2, b_run2) = send_write(
        &env,
        "POST",
        "/api/v1/plugins/builtin.checkin/tasks/check-in/run",
        def_rev + 2,
        "r8-chk-fallback-site",
        json!({ "input": { "site_id": "   " } }),
    )
    .await;
    let run_view2 = assert_ok(st_run2, &b_run2, StatusCode::ACCEPTED);
    let job_id2: domain::JobId = run_view2["job_id"]
        .as_str()
        .or_else(|| run_view2["result"]["job_id"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let job2 = env
        .state
        .jobs()
        .acquire()
        .unwrap()
        .get(job_id2)
        .unwrap()
        .unwrap();
    let err2 = api::plugins::run_task_job(&env.state, &job2).unwrap_err();
    assert!(
        err2.contains("Site URL") && !err2.contains("R8-D-Site-B"),
        "whitespace input.site_id must fall back to config.site_id (site_a only): {err2}"
    );
}

struct MixedStub {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MixedStub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (hits_c, stop_c) = (hits.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                if let Ok((mut s, _)) = listener.accept() {
                    let _ = s.set_nonblocking(false);
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf);
                    let n = hits_c.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 2 {
                        // Drop TCP socket without HTTP response -> host http_fetch Network error ("http request failed: ...")
                        drop(s);
                        continue;
                    }
                    let code = if n == 1 { 400 } else { 200 };
                    let body = b"{\"ok\":true}";
                    let head = format!(
                        "HTTP/1.1 {code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(body);
                    let _ = s.flush();
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            addr,
            hits,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MixedStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

async fn install_mixed_outbox_plugin(env: &HttpEnv, pid: &str, addr: SocketAddr) {
    let empty_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R8 Mixed Outbox"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Mixed permanent + retryable sibling outbox"
config_schema = "config.schema.json"
state_schema = 1

[[http]]
origin = "http://{addr}"
path_prefix = "/webhook"
methods = ["POST"]

[[components]]
id = "comp-perm"
entry = "c1.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:download.after", "events:download.after", "http"]
[[components.hooks]]
id = "download.after"
priority = 10

[[components]]
id = "comp-retry"
entry = "c2.wasm"
world = "{HOOK_WORLD}"
permissions = ["hooks:download.after", "events:download.after", "http"]
[[components.hooks]]
id = "download.after"
priority = 20
"#
    );
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", empty_schema.as_bytes()),
        ("c1.wasm", POLICY_WASM),
        ("c2.wasm", POLICY_WASM),
    ]);
    let grants = json!([
        { "capability": "hooks:download.after", "scope": "Global", "sensitive": false },
        { "capability": "events:download.after", "scope": "Global", "sensitive": false },
        { "capability": "http", "scope": "Global", "sensitive": false }
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r8-mx-en")
        .await;
}

#[tokio::test]
async fn multicomp_checkpoint_preserves_permanent_failure_reason_across_sibling_retry() {
    let stub = MixedStub::start();
    let mut env = HttpEnv::new().await;
    env.state = env.state.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(stub.addr),
    );
    env.app = api::router(env.state.clone());
    env.enable_dev_mode().await;

    let pid = "org.example.r8-d-mixed";
    install_mixed_outbox_plugin(&env, pid, stub.addr).await;

    let t0 = store::unix_now_ms().unwrap();
    let payload = serde_json::to_string(&DownloadAfterEvent {
        operation_id: "op-r8-mx".into(),
        downloader_id: Some("dl-1".into()),
        info_hash: Some("hash-mx".into()),
        outcome: "submitted".into(),
        verified_total_bytes: Some(1024),
    })
    .unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::Subscribe,
            &EmitOutboxEventInput {
                event_id: "ev-r8-mx".into(),
                schema_version: 1,
                event_domain: "download".into(),
                event_kind: "download.after".into(),
                entity_type: "download_operation".into(),
                entity_id: "op-r8-mx".into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: "cause-r8-mx".into(),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"h1\"]".into(),
                payload_json: payload,
                occurred_at: t0,
                created_at: t0,
            },
        )
        .unwrap();

    api::drain_plugin_events_for_test(&env.state, t0).await;
    assert_eq!(stub.hits.load(Ordering::SeqCst), 2);
    let r1 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r8-mx", pid)
        .unwrap()
        .unwrap();
    assert_eq!(r1.status, OutboxDeliveryStatus::Pending);

    api::drain_plugin_events_for_test(&env.state, t0 + 5_000).await;
    let r2 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r8-mx", pid)
        .unwrap()
        .unwrap();
    assert_eq!(r2.status, OutboxDeliveryStatus::DeadLetter);
    let last_err = r2.last_error.unwrap_or_default();
    assert!(
        !last_err.contains("http request failed"),
        "dead-letter last_error must reflect comp-perm's permanent failure, not comp-retry's resolved transient error: `{last_err}`"
    );
}
