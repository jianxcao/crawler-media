use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use net_guard::TargetPolicy;
use plugin_package::{
    MinisignSigningKey, build_deterministic_cmplugin_zip, parse_signing_key, sign_cmplugin_bytes,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{ApiState, Store, router};

const ADMIN_TOKEN: &str = "repo-admin-secret";
const ADMIN_PASS: &str = "repo-admin-pass-123!";
const OFFICIAL_URL: &str =
    "https://raw.githubusercontent.com/jianxcao/crawler-media-plugins/main/registry/index.json";
const TASK_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/task_fixture.wasm");

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct Env {
    _dir: tempfile::TempDir,
    state: ApiState,
    app: axum::Router,
    member: String,
}

impl Env {
    async fn new(policy: Option<TargetPolicy>) -> Self {
        let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
        let store = Arc::new(Store::open(dir.path().join("data")).expect("store"));
        let dl = Arc::new(MemoryDownloader::new(dir.path().join("dl")));
        let snap = RegistrySnapshot::builtin().expect("snap");
        let mut st = ApiState::new_arc_with_admin_password(
            store,
            ADMIN_TOKEN.into(),
            Some(ADMIN_PASS.into()),
            snap,
            Arc::new(OfflineFetcher),
            dl,
        )
        .expect("state");
        if let Some(p) = policy {
            st = st.with_plugin_http_policy_override(p);
        }
        let app = router(st.clone());
        let member = create_member(&app).await;
        Self {
            _dir: dir,
            state: st,
            app,
            member,
        }
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(req).await.expect("oneshot");
        let st = resp.status();
        let b = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let v = serde_json::from_slice(&b)
            .unwrap_or_else(|e| panic!("non-JSON {st} ({e}): {}", String::from_utf8_lossy(&b)));
        (st, v)
    }

    async fn get(&self, uri: &str, tok: &str) -> (StatusCode, Value) {
        self.send(req_json("GET", uri, Some(tok), &[], None)).await
    }

    async fn post(&self, uri: &str, rev: &str, k: &str, body: Value) -> (StatusCode, Value) {
        let idem = format!("{k}-{}", Uuid::new_v4());
        self.send(req_json(
            "POST",
            uri,
            Some(ADMIN_TOKEN),
            &[("if-match", rev), ("idempotency-key", &idem)],
            Some(body),
        ))
        .await
    }

    async fn prep_repo(&self, pid: &str) -> (StatusCode, Value) {
        self.post(
            "/api/v1/plugin-installations/prepare",
            "\"0\"",
            pid,
            json!({ "repo_id": "local", "plugin_id": pid }),
        )
        .await
    }

    async fn cat_status(&self, repo_id: &str) -> StatusCode {
        let u = format!("/api/v1/plugin-repositories/{repo_id}/catalog");
        self.get(&u, ADMIN_TOKEN).await.0
    }

    async fn put_repos(&self, hdrs: &[(&str, &str)], body: Value) -> (StatusCode, Value) {
        self.send(req_json(
            "PUT",
            "/api/v1/plugin-repositories",
            Some(ADMIN_TOKEN),
            hdrs,
            Some(body),
        ))
        .await
    }

    async fn reauth(&self, purpose: &str) -> String {
        let b = Some(json!({ "password": ADMIN_PASS, "purpose": purpose }));
        let (st, v) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-reauth",
                Some(ADMIN_TOKEN),
                &[],
                b,
            ))
            .await;
        assert_eq!(st, StatusCode::OK, "{v:?}");
        v["data"]["reauth_token"].as_str().expect("reauth").into()
    }

    async fn pin_key(&self, key: &MinisignSigningKey, label: &str) {
        let body = Some(json!({ "public_key": key.public_key_text(), "label": label }));
        let (st, bp) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-trust-keys/prepare",
                Some(ADMIN_TOKEN),
                &[],
                body,
            ))
            .await;
        assert_eq!(st, StatusCode::OK, "{bp:?}");
        let p = &bp["data"];
        let reauth = self.reauth("trust-key:create").await;
        let create = json!({ "label": label, "public_key": p["public_key"], "fingerprint": p["fingerprint"], "consent_token": p["consent_token"], "reauth_token": reauth });
        assert_eq!(
            self.post("/api/v1/plugin-trust-keys", "\"0\"", "tk", create)
                .await
                .0,
            StatusCode::CREATED
        );
    }

    async fn enable_dev(&self) {
        let (_, b0) = self.get("/api/v1/settings/plugins", ADMIN_TOKEN).await;
        let rev = format!("\"{}\"", b0["data"]["revision"].as_i64().expect("rev"));
        let tok = self.reauth("developer-mode:enable").await;
        assert_eq!(
            self.post(
                "/api/v1/settings/plugins/developer/enable",
                &rev,
                "dev",
                json!({ "reauth_token": tok })
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    async fn install_upload(&self, pkg: &[u8]) -> Value {
        let uid = Uuid::new_v4().to_string();
        let tok = self.reauth("plugin:upload").await;
        let parts = [
            ("upload_id", None, uid.as_bytes()),
            ("file", Some("pkg.cmplugin"), pkg),
            ("reauth_token", None, tok.as_bytes()),
        ];
        assert_eq!(
            self.send(req_mp(
                "/api/v1/plugin-uploads",
                &format!("up-{uid}"),
                &parts
            ))
            .await
            .0,
            StatusCode::CREATED
        );
        let (sp, bp) = self
            .post(
                "/api/v1/plugin-installations/prepare",
                "\"0\"",
                "pr",
                json!({ "upload_id": uid }),
            )
            .await;
        assert_eq!(sp, StatusCode::ACCEPTED, "{bp:?}");
        let p = &bp["data"]["result"];
        let ei = json!({ "preparation_id": p["preparation_id"], "consent_token": p["consent_token"], "grants": [] });
        let (si, bi) = self
            .post("/api/v1/plugin-installations", "\"0\"", "in", ei)
            .await;
        assert_eq!(
            (si, &bi["data"]["state"]),
            (StatusCode::ACCEPTED, &json!("succeeded"))
        );
        self.plugin(p["plugin_id"].as_str().expect("pid")).await
    }

    async fn save_local(&self, addr: SocketAddr) {
        let b = json!({ "repositories": [{ "id": "local", "name": "Local", "index_url": format!("http://{addr}/index.json"), "enabled": true }] });
        assert_eq!(self.put_repos(&[], b).await.0, StatusCode::OK);
    }

    async fn plugin(&self, pid: &str) -> Value {
        let (st, b) = self
            .get(&format!("/api/v1/plugins/{pid}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b:?}");
        b["data"].clone()
    }

    async fn enable(&self, pid: &str, rev: i64) -> Value {
        let (st, b) = self
            .post(
                &format!("/api/v1/plugins/{pid}/enable"),
                &format!("\"{rev}\""),
                "en",
                json!({}),
            )
            .await;
        assert_eq!(
            (st, &b["data"]["state"]),
            (StatusCode::ACCEPTED, &json!("succeeded"))
        );
        self.plugin(pid).await
    }
}

async fn create_member(app: &axum::Router) -> String {
    let u = json!({ "login": "repo-member", "password": "member-password-123", "role": "member" });
    assert_eq!(
        app.clone()
            .oneshot(req_json(
                "POST",
                "/api/v1/users",
                Some(ADMIN_TOKEN),
                &[],
                Some(u)
            ))
            .await
            .expect("u")
            .status(),
        StatusCode::CREATED
    );
    let cred = json!({ "username": "repo-member", "password": "member-password-123" });
    let resp = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(cred),
        ))
        .await
        .expect("l");
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("b");
    serde_json::from_slice::<Value>(&bytes).expect("json")["data"]["token"]
        .as_str()
        .expect("tok")
        .into()
}

fn req_json(
    m: &str,
    uri: &str,
    tok: Option<&str>,
    hdrs: &[(&str, &str)],
    body: Option<Value>,
) -> Request<Body> {
    let mut b = Request::builder().method(m).uri(uri);
    if let Some(t) = tok {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    for (k, v) in hdrs {
        b = b.header(*k, *v);
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .expect("r"),
        None => b.body(Body::empty()).expect("r"),
    }
}

fn req_mp(uri: &str, idem: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Request<Body> {
    let bd = "----RepoTestBoundary";
    let mut raw = Vec::new();
    for (name, fname, data) in parts {
        raw.extend_from_slice(format!("--{bd}\r\n").as_bytes());
        let h = match fname {
            Some(f) => format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            ),
            None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"),
        };
        raw.extend_from_slice(h.as_bytes());
        raw.extend_from_slice(data);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(format!("--{bd}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
        .header("if-match", "\"0\"")
        .header("idempotency-key", idem)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={bd}"),
        )
        .body(Body::from(raw))
        .expect("mp")
}

fn make_key(byte: u8) -> MinisignSigningKey {
    parse_signing_key(
        &[byte; 32]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
    .expect("key")
}

fn build_pkg(id: &str, ver: &str) -> Vec<u8> {
    let m = format!(
        "manifest_version = 1\nid = \"{id}\"\nname = \"M {id}\"\nversion = \"{ver}\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"T\"]\ndescription = \"T\"\nconfig_schema = \"schemas/c.json\"\nstate_schema = 1\n[[components]]\nid = \"w\"\nentry = \"components/w.wasm\"\nworld = \"crawler:plugin/task-plugin@1.0.0\"\npermissions = [\"kv\"]\n[[components.tasks]]\nid = \"sync\"\ninput_schema = \"schemas/t.json\"\n[components.tasks.schedule]\nkind = \"interval\"\nseconds = 300\n"
    );
    let c = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"mode":{"type":"string","default":"ok"}}}"#;
    let t = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let entries = BTreeMap::from([
        ("plugin.toml".into(), m.into_bytes()),
        ("schemas/c.json".into(), c.as_bytes().to_vec()),
        ("schemas/t.json".into(), t.as_bytes().to_vec()),
        ("components/w.wasm".into(), TASK_WASM.to_vec()),
    ]);
    build_deterministic_cmplugin_zip(&entries).expect("zip")
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

type RouteTable = Vec<(String, &'static str, Vec<u8>)>;
struct DynamicRepoServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    hits: Arc<AtomicUsize>,
    routes: Arc<std::sync::Mutex<RouteTable>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn serve_conn(mut stream: TcpStream, routes: &std::sync::Mutex<RouteTable>, hits: &AtomicUsize) {
    let _ = stream.set_nonblocking(false);
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let path = String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    hits.fetch_add(1, Ordering::Relaxed);
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

impl DynamicRepoServer {
    fn start(initial: RouteTable) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nb");
        let addr = listener.local_addr().expect("addr");
        let (stop, hits, routes) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(std::sync::Mutex::new(initial)),
        );
        let (sb, hb, rb) = (Arc::clone(&stop), Arc::clone(&hits), Arc::clone(&routes));
        let handle = std::thread::spawn(move || {
            while !sb.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => serve_conn(s, &rb, &hb),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            hits,
            routes,
            handle: Some(handle),
        }
    }

    fn set_route(&self, path: &str, ct: &'static str, body: Vec<u8>) {
        let mut g = self.routes.lock().expect("lock");
        if let Some(ex) = g.iter_mut().find(|(p, _, _)| p == path) {
            (ex.1, ex.2) = (ct, body);
        } else {
            g.push((path.into(), ct, body));
        }
    }
}

impl Drop for DynamicRepoServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn uploaded_ids(env: &Env) -> Vec<String> {
    std::fs::read_dir(env.state.store().data_dir().join("plugins/uploads"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn cat_item(
    id: &str,
    name: &str,
    ver: &str,
    pkg: String,
    sig: Option<String>,
    sha: String,
) -> Value {
    json!({ "id": id, "name": name, "version": ver, "description": "desc", "authors": ["T"], "license": "MIT", "package_url": pkg, "signature_url": sig, "sha256": sha, "capabilities": ["kv"], "homepage": "https://example.com/p" })
}

#[tokio::test]
async fn repositories_default_official_persistence_and_validation() {
    let env = Env::new(None).await;
    assert_eq!(
        env.get("/api/v1/plugin-repositories", &env.member).await.0,
        StatusCode::FORBIDDEN
    );
    let (st0, b0) = env.get("/api/v1/plugin-repositories", ADMIN_TOKEN).await;
    assert_eq!(st0, StatusCode::OK, "{b0:?}");
    let r0 = b0["data"]["repositories"].as_array().expect("repos");
    assert_eq!(
        r0,
        &[
            json!({ "id": "official", "name": "crawler-media Official Plugins", "index_url": OFFICIAL_URL, "enabled": true, "builtin": true })
        ]
    );

    for bad in [
        json!({ "repositories": [{ "id": "local", "name": "L", "index_url": "http://127.0.0.1:8080/i.json", "enabled": true }] }),
        json!({ "repositories": [{ "id": "http", "name": "H", "index_url": "http://example.com/i.json", "enabled": true }] }),
        json!({ "repositories": [{ "id": "../evil", "name": "B", "index_url": "https://example.com/i.json", "enabled": true }] }),
        json!({ "repositories": [{ "id": "BadUpper", "name": "B", "index_url": "https://example.com/i.json", "enabled": true }] }),
        json!({ "repositories": [{ "id": "dup", "name": "1", "index_url": "https://a.example/i.json", "enabled": true }, { "id": "dup", "name": "2", "index_url": "https://b.example/i.json", "enabled": true }] }),
        json!({ "repositories": [{ "id": "official", "name": "Tampered", "index_url": "https://evil.example/i.json", "enabled": true }] }),
    ] {
        assert_eq!(env.put_repos(&[], bad).await.0, StatusCode::BAD_REQUEST);
    }

    let save = json!({ "repositories": [
        { "id": "official", "name": "crawler-media Official Plugins", "index_url": OFFICIAL_URL, "enabled": false, "builtin": true },
        { "id": "community", "name": "Community", "index_url": "https://plugins.example.com/index.json", "enabled": true, "builtin": false }
    ] });
    let (st_s, bs) = env
        .put_repos(&[("idempotency-key", "save-repos-1")], save)
        .await;
    assert_eq!(st_s, StatusCode::OK, "{bs:?}");
    let saved = bs["data"]["repositories"].as_array().expect("saved");
    assert_eq!(
        (
            saved.len(),
            saved[0]["enabled"].as_bool(),
            saved[1]["builtin"].as_bool()
        ),
        (2, Some(false), Some(false))
    );
    assert!(
        env.state
            .store()
            .get_setting("plugin.repositories")
            .expect("kv")
            .expect("set")
            .contains("community")
    );
    assert_eq!(
        env.get("/api/v1/plugin-repositories", ADMIN_TOKEN).await.1["data"]["repositories"],
        bs["data"]["repositories"]
    );

    let env_t = Env::new(Some(TargetPolicy::production())).await;
    let loc = json!({ "repositories": [
        { "id": "v4", "name": "IPv4", "index_url": "http://127.0.0.1:18080/index.json", "enabled": true },
        { "id": "v6", "name": "IPv6", "index_url": "http://[::1]:18081/index.json", "enabled": true }
    ] });
    let (st_l, bl) = env_t.put_repos(&[], loc).await;
    assert_eq!(
        (st_l, bl["data"]["repositories"].as_array().unwrap().len()),
        (StatusCode::OK, 3)
    );
}

#[tokio::test]
async fn repositories_catalog_merges_installed_version_and_semver_updates() {
    let (pkg_a0, pkg_b, pkg_a1, pkg_c) = (
        build_pkg("org.example.cat-a", "1.0.0"),
        build_pkg("org.example.cat-b", "1.0.0"),
        build_pkg("org.example.cat-a", "1.2.0"),
        build_pkg("org.example.cat-c", "2.0.0"),
    );
    let index = json!({ "schema_version": 1, "generated_at": "2026-11-27T00:00:00Z", "repository": { "id": "local", "name": "Local Test Repo" }, "plugins": [
        cat_item("org.example.cat-a", "Plugin A", "1.2.0", "https://cdn.example.com/a-1.2.0.cmplugin".into(), Some("https://cdn.example.com/a.minisig".into()), sha256_hex(&pkg_a1)),
        cat_item("org.example.cat-b", "Plugin B", "1.0.0", "https://cdn.example.com/b-1.0.0.cmplugin".into(), None, sha256_hex(&pkg_b)),
        cat_item("org.example.cat-c", "Plugin C", "2.0.0", "https://cdn.example.com/c-2.0.0.cmplugin".into(), None, sha256_hex(&pkg_c)),
    ] });
    let srv = DynamicRepoServer::start(vec![
        (
            "/index.json".into(),
            "application/json",
            index.to_string().into_bytes(),
        ),
        (
            "/bad.json".into(),
            "application/json",
            br#"{"schema_version":2,"plugins":[]}"#.to_vec(),
        ),
        (
            "/huge.json".into(),
            "application/json",
            vec![b' '; 2 * 1024 * 1024 + 64],
        ),
    ]);
    let env = Env::new(Some(
        TargetPolicy::production().with_test_loopback_socket(srv.addr),
    ))
    .await;
    env.enable_dev().await;
    let det_a = env.install_upload(&pkg_a0).await;
    env.enable("org.example.cat-a", det_a["revision"].as_i64().unwrap())
        .await;
    env.install_upload(&pkg_b).await;

    let a = srv.addr;
    let repos = json!({ "repositories": [
        { "id": "local", "name": "Local Test Repo", "index_url": format!("http://{a}/index.json"), "enabled": true },
        { "id": "disabled-repo", "name": "Dis", "index_url": format!("http://{a}/index.json"), "enabled": false },
        { "id": "bad-schema", "name": "Bad", "index_url": format!("http://{a}/bad.json"), "enabled": true },
        { "id": "huge", "name": "Huge", "index_url": format!("http://{a}/huge.json"), "enabled": true }
    ] });
    assert_eq!(env.put_repos(&[], repos).await.0, StatusCode::OK);

    let (st, bc) = env
        .get("/api/v1/plugin-repositories/local/catalog", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{bc:?}");
    assert_eq!(bc["data"]["repo_id"], "local");
    assert_eq!(bc["data"]["repo_name"], "Local Test Repo");
    let pl = bc["data"]["plugins"].as_array().expect("plugins");
    assert_eq!(pl.len(), 3);
    assert_eq!(pl[0]["installed_version"], "1.0.0");
    assert_eq!(pl[0]["lifecycle"], "enabled");
    assert_eq!(pl[0]["update_available"], true);
    assert_eq!(pl[1]["installed_version"], "1.0.0");
    assert_eq!(pl[1]["lifecycle"], "installed-disabled");
    assert_eq!(pl[1]["update_available"], false);
    assert_eq!(pl[2]["installed_version"], Value::Null);
    assert_eq!(pl[2]["lifecycle"], Value::Null);
    assert_eq!(pl[2]["update_available"], false);
    assert_eq!(env.cat_status("missing").await, StatusCode::NOT_FOUND);
    assert_eq!(env.cat_status("disabled-repo").await, StatusCode::CONFLICT);
    assert_eq!(
        env.cat_status("bad-schema").await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(env.cat_status("huge").await, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(srv.hits.load(Ordering::Relaxed) >= 3);
}

#[tokio::test]
async fn repositories_prepare_install_rejects_tampered_sha_untrusted_sig_and_oversize() {
    let (trusted, untrusted) = (make_key(0x31), make_key(0x99));
    let pkg = build_pkg("org.example.untrusted", "1.0.0");
    let sig_t = sign_cmplugin_bytes(&pkg, &trusted, "p.cmplugin").expect("sign");
    let sig_u = sign_cmplugin_bytes(&pkg, &untrusted, "p.cmplugin").expect("sign");
    let huge = vec![0x50u8; 16 * 1024 * 1024 + 64];
    let srv = DynamicRepoServer::start(vec![
        (
            "/pkg.cmplugin".into(),
            "application/octet-stream",
            pkg.clone(),
        ),
        ("/trusted.minisig".into(), "text/plain", sig_t.into_bytes()),
        (
            "/untrusted.minisig".into(),
            "text/plain",
            sig_u.into_bytes(),
        ),
        (
            "/bigsig.minisig".into(),
            "text/plain",
            vec![b'x'; 16 * 1024 + 64],
        ),
        (
            "/huge.cmplugin".into(),
            "application/octet-stream",
            huge.clone(),
        ),
    ]);
    let a = srv.addr;
    let idx = json!({ "schema_version": 1, "plugins": [
        cat_item("org.example.tampered", "T", "1.0.0", format!("http://{a}/pkg.cmplugin"), Some(format!("http://{a}/trusted.minisig")), "f".repeat(64)),
        cat_item("org.example.untrusted", "U", "1.0.0", format!("http://{a}/pkg.cmplugin"), Some(format!("http://{a}/untrusted.minisig")), sha256_hex(&pkg)),
        cat_item("org.example.bigsig", "B", "1.0.0", format!("http://{a}/pkg.cmplugin"), Some(format!("http://{a}/bigsig.minisig")), sha256_hex(&pkg)),
        cat_item("org.example.hugepkg", "H", "1.0.0", format!("http://{a}/huge.cmplugin"), Some(format!("http://{a}/trusted.minisig")), sha256_hex(&huge)),
    ] });
    srv.set_route(
        "/index.json",
        "application/json",
        idx.to_string().into_bytes(),
    );

    let env = Env::new(Some(
        TargetPolicy::production().with_test_loopback_socket(a),
    ))
    .await;
    env.pin_key(&trusted, "Official Key").await;
    env.save_local(a).await;

    let (st_t, bt) = env.prep_repo("org.example.tampered").await;
    assert_eq!(st_t, StatusCode::BAD_REQUEST, "{bt:?}");
    assert!(
        uploaded_ids(&env).is_empty(),
        "tampered sha256 must fail before staging"
    );

    let (st_u, bu) = env.prep_repo("org.example.untrusted").await;
    assert_eq!(
        (st_u, bu["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("plugin.untrusted_key"))
    );
    assert!(uploaded_ids(&env).is_empty());

    assert_eq!(
        env.prep_repo("org.example.bigsig").await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        env.prep_repo("org.example.hugepkg").await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn repositories_install_and_upgrade_signed_package_end_to_end() {
    let trusted = make_key(0x31);
    let (pkg1, pkg2) = (
        build_pkg("org.example.mkt", "1.0.0"),
        build_pkg("org.example.mkt", "1.1.0"),
    );
    let (sig1, sig2) = (
        sign_cmplugin_bytes(&pkg1, &trusted, "m1.cmplugin").expect("s1"),
        sign_cmplugin_bytes(&pkg2, &trusted, "m2.cmplugin").expect("s2"),
    );
    let srv = DynamicRepoServer::start(vec![
        (
            "/m1.cmplugin".into(),
            "application/octet-stream",
            pkg1.clone(),
        ),
        ("/m1.minisig".into(), "text/plain", sig1.into_bytes()),
        (
            "/m2.cmplugin".into(),
            "application/octet-stream",
            pkg2.clone(),
        ),
        ("/m2.minisig".into(), "text/plain", sig2.into_bytes()),
    ]);
    let a = srv.addr;
    let idx1 = json!({ "schema_version": 1, "plugins": [cat_item("org.example.mkt", "Market", "1.0.0", format!("http://{a}/m1.cmplugin"), Some(format!("http://{a}/m1.minisig")), sha256_hex(&pkg1))] });
    srv.set_route(
        "/index.json",
        "application/json",
        idx1.to_string().into_bytes(),
    );

    let env = Env::new(Some(
        TargetPolicy::production().with_test_loopback_socket(a),
    ))
    .await;
    env.pin_key(&trusted, "Official Key").await;
    env.save_local(a).await;

    let (st_p1, bp1) = env.prep_repo("org.example.mkt").await;
    assert_eq!(st_p1, StatusCode::ACCEPTED, "{bp1:?}");
    let p1 = &bp1["data"]["result"];
    assert_eq!(
        (&p1["version"], &p1["signature_status"]),
        (&json!("1.0.0"), &json!("signed"))
    );
    let ei = json!({ "preparation_id": p1["preparation_id"], "consent_token": p1["consent_token"], "grants": [] });
    let (st_i1, bi1) = env
        .post("/api/v1/plugin-installations", "\"0\"", "ie1", ei)
        .await;
    assert_eq!(
        (st_i1, &bi1["data"]["state"]),
        (StatusCode::ACCEPTED, &json!("succeeded"))
    );

    let det1 = env.plugin("org.example.mkt").await;
    let det2 = env
        .enable("org.example.mkt", det1["revision"].as_i64().unwrap())
        .await;
    let if_rev2 = format!("\"{}\"", det2["revision"].as_i64().unwrap());

    let idx2 = json!({ "schema_version": 1, "plugins": [cat_item("org.example.mkt", "Market", "1.1.0", format!("http://{a}/m2.cmplugin"), Some(format!("http://{a}/m2.minisig")), sha256_hex(&pkg2))] });
    srv.set_route(
        "/index.json",
        "application/json",
        idx2.to_string().into_bytes(),
    );

    let (st_up, bup) = env
        .post(
            "/api/v1/plugins/org.example.mkt/upgrades/prepare",
            &if_rev2,
            "up",
            json!({ "repo_id": "local", "state_policy": "preserve" }),
        )
        .await;
    assert_eq!(st_up, StatusCode::ACCEPTED, "{bup:?}");
    let up = &bup["data"]["result"];
    assert_eq!(
        (&up["version"], &up["signature_status"]),
        (&json!("1.1.0"), &json!("signed"))
    );
    let eu = json!({ "preparation_id": up["preparation_id"], "consent_token": up["consent_token"], "grants": [], "state_policy": "preserve", "target_config_digest": up["config_diff"]["target_config_digest"] });
    let (st_ue, bue) = env
        .post(
            "/api/v1/plugins/org.example.mkt/upgrade",
            &if_rev2,
            "ue",
            eu,
        )
        .await;
    assert_eq!(
        (st_ue, &bue["data"]["state"]),
        (StatusCode::ACCEPTED, &json!("succeeded"))
    );
    assert_eq!(env.plugin("org.example.mkt").await["version"], "1.1.0");
}
