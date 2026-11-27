#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use domain::SiteId;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, req_json};
use serde_json::{Value, json};
use store::{SiteMaintenanceReceiptFilter, SiteMaintenanceState};

struct StubServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start(handler: impl Fn() -> (u16, String) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let (code, body) = handler();
                        let reason = if code == 200 { "OK" } else { "ERR" };
                        let resp = format!(
                            "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct LocalLoopbackFetcher;

impl indexer::Fetcher for LocalLoopbackFetcher {
    fn fetch(&self, request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        let parsed = url::Url::parse(&request.url)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port_or_known_default().unwrap_or(80);
        let path = match parsed.query() {
            Some(q) => format!("{}?{q}", parsed.path()),
            None => parsed.path().to_string(),
        };
        let mut stream = std::net::TcpStream::connect((host, port))
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let mut buf = String::new();
        stream
            .read_to_string(&mut buf)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        Ok(buf
            .split_once("\r\n\r\n")
            .map_or(buf.as_str(), |(_, b)| b)
            .to_string())
    }
}

async fn live_http_env() -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = api::Store::open(dir.path().join("data")).unwrap();
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
    let state = api::ApiState::new_arc_with_admin_password(
        Arc::new(store),
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(LocalLoopbackFetcher),
        dl,
    )
    .unwrap();
    let app = api::router(state.clone());
    HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    }
}

async fn create_site(env: &HttpEnv, url: &str) -> SiteId {
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "NexusTest",
                "url": url,
                "profile_id": "pterclub",
                "cookie": "uid=1; pass=initial",
                "maintenance_via": "http",
                "timezone": "UTC",
                "enabled": true,
            })),
        ))
        .await;
    let data: Value = assert_ok(st, &body, StatusCode::CREATED);
    data["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn checkin_emits_site_checkin_after_notify_after_receipt_commit_and_isolates_failure() {
    let env = live_http_env().await;
    env.enable_dev_mode().await;

    let policy_bytes = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");
    let schema = plugin_http_support::pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "org.example.checkin-notify"
name = "Checkin Notify"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Notify fixture"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "policy"
entry = "policy.wasm"
world = "{}"
permissions = ["hooks:site.checkin.after", "events:site.checkin.after"]

[[components.hooks]]
id = "site.checkin.after"
priority = 10
"#,
        plugin_api::HOOK_WORLD
    );
    let pkg_bytes = plugin_http_support::pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", policy_bytes),
    ]);
    let installed = env
        .install_pkg(
            &pkg_bytes,
            None,
            json!([
                { "capability": "hooks:site.checkin.after", "scope": "Global", "sensitive": false },
                { "capability": "events:site.checkin.after", "scope": "Global", "sensitive": false }
            ]),
        )
        .await;
    let rev = installed["revision"].as_i64().unwrap();
    env.enable_plugin("org.example.checkin-notify", rev, "en-checkin-notify")
        .await;

    let srv_ok = StubServer::start(|| {
        (
            200,
            "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>".into(),
        )
    });
    let s1_id = create_site(&env, &srv_ok.url()).await;

    api::check_in_run_for_test(&env.state).unwrap();
    api::check_in_run_for_test(&env.state).unwrap();
    api::drain_plugin_events_for_test(&env.state, store::unix_now_ms().unwrap()).await;
    let traces = env.state.plugins().take_recent_checkin_traces_for_test();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].hook_id, "site.checkin.after");
    assert_eq!(traces[0].decision, "notified");

    let srv_fail = StubServer::start(|| (500, "server error".into()));
    let s2_id = create_site(&env, &srv_fail.url()).await;
    let srv_unk = StubServer::start(|| (200, "<html><body>plain</body></html>".into()));
    let s3_id = create_site(&env, &srv_unk.url()).await;

    assert!(api::check_in_run_for_test(&env.state).is_err());
    api::drain_plugin_events_for_test(&env.state, store::unix_now_ms().unwrap()).await;
    let traces2 = env.state.plugins().take_recent_checkin_traces_for_test();
    assert!(
        traces2.len() >= 2,
        "expected traces for check-ins: {traces2:?}"
    );

    for (site_id, expected) in [
        (s1_id, SiteMaintenanceState::Completed),
        (s2_id, SiteMaintenanceState::Unknown),
        (s3_id, SiteMaintenanceState::Failed),
    ] {
        let receipts = env
            .state
            .store()
            .list_site_maintenance_receipts(
                &SiteMaintenanceReceiptFilter {
                    site_id: Some(site_id),
                    ..Default::default()
                },
                None,
                Some(10),
            )
            .unwrap();
        assert_eq!(receipts.items[0].state, expected);
    }
}
