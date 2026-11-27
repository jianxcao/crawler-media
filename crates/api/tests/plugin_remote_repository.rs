#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::{ApiState, router};
use axum::http::StatusCode;
use net_guard::TargetPolicy;
use plugin_http_support::{
    HttpEnv, TOKEN, assert_err, assert_ok, assert_paged, build_state,
    pkg::{TestSigningKey, build_task_package, schema_rev},
    req_json,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const OFFICIAL_INDEX_URL: &str =
    "https://raw.githubusercontent.com/jianxcao/crawler-media-plugins/main/registry/index.json";

type RouteTable = Vec<(String, &'static str, Vec<u8>)>;

struct StubRepoServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    routes: Arc<std::sync::Mutex<RouteTable>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn handle_conn(mut stream: TcpStream, routes: &std::sync::Mutex<RouteTable>) {
    let _ = stream.set_nonblocking(false);
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let path = String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let hit = routes
        .lock()
        .expect("lock")
        .iter()
        .find(|(p, _, _)| p == &path)
        .map(|(_, ct, b)| (*ct, b.clone()));
    match hit {
        Some((ct, body)) => {
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(hdr.as_bytes());
            let _ = stream.write_all(&body);
        }
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
}

impl StubRepoServer {
    fn start(initial: RouteTable) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nb");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let routes = Arc::new(std::sync::Mutex::new(initial));
        let (sb, rb) = (Arc::clone(&stop), Arc::clone(&routes));
        let handle = std::thread::spawn(move || {
            while !sb.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => handle_conn(s, &rb),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            routes,
            handle: Some(handle),
        }
    }

    fn set_route(&self, path: &str, ct: &'static str, body: Vec<u8>) {
        let mut guard = self.routes.lock().expect("lock");
        if let Some(ex) = guard.iter_mut().find(|(p, _, _)| p == path) {
            (ex.1, ex.2) = (ct, body);
        } else {
            guard.push((path.to_string(), ct, body));
        }
    }
}

impl Drop for StubRepoServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_env_with_loopback(addr: SocketAddr) -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let state: ApiState = build_state(dir.path()).with_plugin_http_policy_override(
        TargetPolicy::production().with_test_loopback_socket(addr),
    );
    let app = router(state.clone());
    HttpEnv {
        _dir: dir,
        state,
        app,
        member_token: String::new(),
    }
}

fn catalog_plugin(
    id: &str,
    name: &str,
    version: &str,
    pkg_url: String,
    sig_url: String,
    sha256: String,
) -> Value {
    json!({
        "id": id,
        "name": name,
        "version": version,
        "description": "Integration test plugin",
        "authors": ["RepoTester"],
        "license": "MIT",
        "package_url": pkg_url,
        "signature_url": sig_url,
        "sha256": sha256,
        "capabilities": ["kv"],
        "homepage": "https://example.com/plugin"
    })
}

async fn setup_local_repo(env: &HttpEnv, addr: SocketAddr, key: &TestSigningKey) {
    env.pin_trust_key(key, "Stub Signing Key").await;
    let body = json!({
        "repositories": [{
            "id": "local",
            "name": "Local Stub",
            "index_url": format!("http://{addr}/index.json"),
            "enabled": true
        }]
    });
    let (st, _, raw) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugin-repositories",
            Some(TOKEN),
            &[],
            Some(body),
        ))
        .await;
    assert_ok(st, &raw, StatusCode::OK);
}

#[tokio::test]
async fn plugin_repositories_crud_and_official_trust_key_seeded() {
    let env = HttpEnv::new().await;
    let (st_mem, _, _) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-repositories",
            Some(&env.member_token),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_mem, StatusCode::FORBIDDEN);

    let (st0, _, b0) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-repositories",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let repos = assert_ok(st0, &b0, StatusCode::OK)["repositories"]
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["id"], "official");
    assert_eq!(repos[0]["index_url"], OFFICIAL_INDEX_URL);
    assert_eq!(repos[0]["builtin"], true);

    let (st_tk, _, b_tk) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-trust-keys",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (keys, _) = assert_paged(st_tk, &b_tk);
    assert!(
        keys.iter().any(|k| {
            k["label"] == "crawler-media Official Plugins" && k["status"] == "trusted"
        })
    );

    let save = json!({
        "repositories": [{
            "id": "community",
            "name": "Community",
            "index_url": "https://plugins.example.com/index.json",
            "enabled": true
        }]
    });
    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugin-repositories",
            Some(TOKEN),
            &[("idempotency-key", "put-custom-1")],
            Some(save),
        ))
        .await;
    let list = assert_ok(st_put, &b_put, StatusCode::OK)["repositories"]
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1]["id"], "community");
    assert_eq!(list[1]["builtin"], false);
}

#[tokio::test]
async fn plugin_repositories_reject_tampered_sha_and_untrusted_sig() {
    let trusted = TestSigningKey::generate(51);
    let untrusted = TestSigningKey::generate(99);
    let pkg = build_task_package("org.example.remote", "1.0.0", 1, false);
    let sig_ok = trusted.sign_package(&pkg);
    let sig_bad = untrusted.sign_package(&pkg);
    let srv = StubRepoServer::start(vec![
        (
            "/v1.cmplugin".into(),
            "application/octet-stream",
            pkg.clone(),
        ),
        ("/v1.minisig".into(), "text/plain", sig_ok.into_bytes()),
        ("/v1-bad.minisig".into(), "text/plain", sig_bad.into_bytes()),
    ]);
    let a = srv.addr;
    let idx = json!({
        "schema_version": 1,
        "plugins": [
            catalog_plugin("org.example.tampered", "T", "1.0.0", format!("http://{a}/v1.cmplugin"), format!("http://{a}/v1.minisig"), "0".repeat(64)),
            catalog_plugin("org.example.untrusted", "U", "1.0.0", format!("http://{a}/v1.cmplugin"), format!("http://{a}/v1-bad.minisig"), sha256_hex(&pkg)),
        ]
    });
    srv.set_route(
        "/index.json",
        "application/json",
        idx.to_string().into_bytes(),
    );

    let env = build_env_with_loopback(a);
    setup_local_repo(&env, a, &trusted).await;

    let (st_t, _, b_t) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "it-tamp")],
            Some(json!({ "repo_id": "local", "plugin_id": "org.example.tampered" })),
        ))
        .await;
    assert_err(
        st_t,
        &b_t,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_package",
    );

    let (st_u, _, b_u) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "it-untr")],
            Some(json!({ "repo_id": "local", "plugin_id": "org.example.untrusted" })),
        ))
        .await;
    assert_err(st_u, &b_u, StatusCode::FORBIDDEN, "plugin.untrusted_key");
}

#[tokio::test]
async fn plugin_repositories_catalog_install_and_upgrade_flow() {
    let trusted = TestSigningKey::generate(52);
    let pkg1 = build_task_package("org.example.remote", "1.0.0", 1, false);
    let sig1 = trusted.sign_package(&pkg1);
    let pkg2 = build_task_package("org.example.remote", "1.1.0", 1, false);
    let sig2 = trusted.sign_package(&pkg2);
    let srv = StubRepoServer::start(vec![
        (
            "/v1.cmplugin".into(),
            "application/octet-stream",
            pkg1.clone(),
        ),
        ("/v1.minisig".into(), "text/plain", sig1.into_bytes()),
        (
            "/v2.cmplugin".into(),
            "application/octet-stream",
            pkg2.clone(),
        ),
        ("/v2.minisig".into(), "text/plain", sig2.into_bytes()),
    ]);
    let a = srv.addr;
    let idx1 = json!({ "schema_version": 1, "plugins": [
        catalog_plugin("org.example.remote", "Remote", "1.0.0", format!("http://{a}/v1.cmplugin"), format!("http://{a}/v1.minisig"), sha256_hex(&pkg1)),
    ] });
    srv.set_route(
        "/index.json",
        "application/json",
        idx1.to_string().into_bytes(),
    );

    let env = build_env_with_loopback(a);
    setup_local_repo(&env, a, &trusted).await;

    let (st_p1, _, b_p1) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "it-p1")],
            Some(json!({ "repo_id": "local", "plugin_id": "org.example.remote" })),
        ))
        .await;
    let prep1 = &assert_ok(st_p1, &b_p1, StatusCode::ACCEPTED)["result"];
    let (st_e1, _, b_e1) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "it-e1")],
            Some(json!({ "preparation_id": prep1["preparation_id"], "consent_token": prep1["consent_token"], "grants": [] })),
        ))
        .await;
    assert_eq!(
        assert_ok(st_e1, &b_e1, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );

    let rev1 = env.get_plugin_rev("org.example.remote").await;
    env.enable_plugin("org.example.remote", rev1, "it-en").await;
    let rev2 = env.get_plugin_rev("org.example.remote").await;

    let idx2 = json!({ "schema_version": 1, "plugins": [
        catalog_plugin("org.example.remote", "Remote", "1.1.0", format!("http://{a}/v2.cmplugin"), format!("http://{a}/v2.minisig"), sha256_hex(&pkg2)),
    ] });
    srv.set_route(
        "/index.json",
        "application/json",
        idx2.to_string().into_bytes(),
    );

    let (st_cat, _, b_cat) = env
        .send(req_json(
            "GET",
            "/api/v1/plugin-repositories/local/catalog",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cat = assert_ok(st_cat, &b_cat, StatusCode::OK);
    assert_eq!(cat["plugins"][0]["installed_version"], "1.0.0");
    assert_eq!(cat["plugins"][0]["lifecycle"], "enabled");
    assert_eq!(cat["plugins"][0]["update_available"], true);

    let srev = schema_rev(false, None);
    let (st_up, _, b_up) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.remote/upgrades/prepare",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev2}\"")),
                ("idempotency-key", "it-up"),
            ],
            Some(json!({
                "repo_id": "local",
                "state_policy": "preserve",
                "target_config": { "schema_revision": srev, "values": { "mode": "v2" } },
            })),
        ))
        .await;
    let up_prep = &assert_ok(st_up, &b_up, StatusCode::ACCEPTED)["result"];
    assert_eq!(up_prep["version"], "1.1.0");
    assert_eq!(up_prep["signature_status"], "signed");
}
