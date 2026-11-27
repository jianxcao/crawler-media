#![allow(dead_code)]

#[path = "../plugin_p1c_support/pkg.rs"]
pub mod pkg;

use std::str::FromStr;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use domain::{Site, SiteId, UserId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use pkg::TestSigningKey;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

pub const TOKEN: &str = "management-secret";
pub const ADMIN_PASS: &str = "p1c-admin-pass-123!";

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

pub fn admin_id() -> UserId {
    UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap()
}

pub fn build_state(dir: &std::path::Path) -> ApiState {
    let store = Store::open(dir.join("data")).unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.join("stage")));
    let state = ApiState::new_arc_with_admin_password(
        Arc::new(store),
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        dl,
    )
    .unwrap();
    let jobs = state.jobs();
    let queue = jobs.acquire().unwrap();
    for def in queue.list_defs().unwrap() {
        if !def.kind.is_plugin() {
            let _ = queue.set_def_enabled_by_id(def.id, false);
        }
    }
    state
}

pub struct HttpEnv {
    pub _dir: tempfile::TempDir,
    pub state: ApiState,
    pub app: axum::Router,
    pub member_token: String,
}

impl HttpEnv {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let state = build_state(dir.path());
        let app = router(state.clone());
        let member_token = create_member(&app, "plugin-member").await;
        Self {
            _dir: dir,
            state,
            app,
            member_token,
        }
    }

    pub async fn send(&self, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "response status={status} is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, headers, body)
    }

    pub fn insert_site(&self, name: &str) -> SiteId {
        let id = SiteId::new();
        self.state
            .store()
            .insert_site(&Site {
                id,
                name: name.into(),
                url: "https://pt.example/".into(),
                profile_id: "demo".into(),
                cookie: Some("uid=1; pass=site-secret-cookie".into()),
                api_key: None,
                rss_url: None,
                proxy: None,
                rate_limit_per_minute: Some(12),
                cdp_url: None,
                downloader_id: None,
                enabled: true,
                maintenance_via: domain::MaintenanceVia::Http,
                browser_search: false,
                timezone: "UTC".into(),
            })
            .unwrap();
        id
    }

    pub fn insert_downloader(&self, name: &str) -> domain::DownloaderId {
        let id = domain::DownloaderId::new();
        self.state
            .store()
            .insert_downloader(&store::DownloaderRow {
                id,
                name: name.into(),
                kind: "qbittorrent".into(),
                url: "http://127.0.0.1:9".into(),
                username: None,
                password: None,
                api_key: None,
                category: None,
                path_maps: vec![],
                is_default: false,
                enabled: true,
            })
            .unwrap();
        id
    }

    pub async fn reauth(&self, purpose: &str) -> String {
        let (st, _, body) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-reauth",
                Some(TOKEN),
                &[],
                Some(json!({ "password": ADMIN_PASS, "purpose": purpose })),
            ))
            .await;
        let data = assert_ok(st, &body, StatusCode::OK);
        data["reauth_token"].as_str().unwrap().to_string()
    }

    pub async fn enable_dev_mode(&self) -> Value {
        let (st0, _, b0) = self
            .send(req_json(
                "GET",
                "/api/v1/settings/plugins",
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let s0 = assert_ok(st0, &b0, StatusCode::OK);
        let rev = s0["revision"].as_i64().unwrap();
        let reauth = self.reauth("developer-mode:enable").await;
        let if_match = format!("\"{rev}\"");
        let idem = format!("dev-en-{}", Uuid::new_v4());
        let (st, _, body) = self
            .send(req_json(
                "POST",
                "/api/v1/settings/plugins/developer/enable",
                Some(TOKEN),
                &[("if-match", &if_match), ("idempotency-key", &idem)],
                Some(json!({ "reauth_token": reauth })),
            ))
            .await;
        assert_ok(st, &body, StatusCode::OK)
    }

    pub async fn pin_trust_key(&self, key: &TestSigningKey, label: &str) -> Value {
        let (st_p, _, b_p) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-trust-keys/prepare",
                Some(TOKEN),
                &[],
                Some(json!({ "public_key": key.public_key_str, "label": label })),
            ))
            .await;
        let prep = assert_ok(st_p, &b_p, StatusCode::OK);
        let reauth = self.reauth("trust-key:create").await;
        let idem = format!("tk-create-{}", Uuid::new_v4());
        let (st_c, _, b_c) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-trust-keys",
                Some(TOKEN),
                &[("if-match", "\"0\""), ("idempotency-key", &idem)],
                Some(json!({
                    "label": label,
                    "public_key": prep["public_key"],
                    "fingerprint": prep["fingerprint"],
                    "consent_token": prep["consent_token"],
                    "reauth_token": reauth,
                })),
            ))
            .await;
        assert_ok(st_c, &b_c, StatusCode::CREATED)
    }

    pub async fn upload_pkg(&self, pkg: &[u8], sig: Option<&str>, key: &str) -> Value {
        let upload_id = Uuid::new_v4().to_string();
        let reauth = self.reauth("plugin:upload").await;
        let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![
            ("upload_id", None, upload_id.as_bytes()),
            ("file", Some("plugin.cmplugin"), pkg),
            ("reauth_token", None, reauth.as_bytes()),
        ];
        if let Some(s) = sig {
            parts.push(("signature", Some("plugin.minisig"), s.as_bytes()));
        }
        let req = req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", key)],
            "----PluginHttpTestBoundary",
            &parts,
        );
        let (st, _, body) = self.send(req).await;
        assert_ok(st, &body, StatusCode::CREATED)
    }

    pub async fn install_pkg(&self, pkg: &[u8], sig: Option<&str>, grants: Value) -> Value {
        let up_key = format!("up-{}", Uuid::new_v4());
        let up = self.upload_pkg(pkg, sig, &up_key).await;
        let upload_id = up["upload_id"].as_str().unwrap();
        let prep_key = format!("prep-{}", Uuid::new_v4());
        let (st_p, _, b_p) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-installations/prepare",
                Some(TOKEN),
                &[("if-match", "\"0\""), ("idempotency-key", &prep_key)],
                Some(json!({ "upload_id": upload_id })),
            ))
            .await;
        let prep_op = assert_ok(st_p, &b_p, StatusCode::ACCEPTED);
        assert_eq!(prep_op["state"], "succeeded");
        let prep = &prep_op["result"];
        let inst_key = format!("inst-{}", Uuid::new_v4());
        let (st_i, _, b_i) = self
            .send(req_json(
                "POST",
                "/api/v1/plugin-installations",
                Some(TOKEN),
                &[("if-match", "\"0\""), ("idempotency-key", &inst_key)],
                Some(json!({
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants,
                })),
            ))
            .await;
        let inst_op = assert_ok(st_i, &b_i, StatusCode::ACCEPTED);
        assert_eq!(inst_op["state"], "succeeded");
        let pid = prep["plugin_id"].as_str().unwrap();
        let (st_g, _, b_g) = self
            .send(req_json(
                "GET",
                &format!("/api/v1/plugins/{pid}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        assert_ok(st_g, &b_g, StatusCode::OK)
    }

    pub async fn get_plugin_rev(&self, pid: &str) -> i64 {
        let (st, _, b) = self
            .send(req_json(
                "GET",
                &format!("/api/v1/plugins/{pid}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        assert_ok(st, &b, StatusCode::OK)["revision"]
            .as_i64()
            .unwrap()
    }

    pub async fn enable_plugin(&self, pid: &str, rev: i64, key: &str) -> Value {
        let if_match = format!("\"{rev}\"");
        let (st, _, body) = self
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/enable"),
                Some(TOKEN),
                &[("if-match", &if_match), ("idempotency-key", key)],
                Some(json!({})),
            ))
            .await;
        let op = assert_ok(st, &body, StatusCode::ACCEPTED);
        assert_eq!(op["state"], "succeeded");
        op
    }

    pub async fn disable_plugin(&self, pid: &str, rev: i64, key: &str) -> Value {
        let if_match = format!("\"{rev}\"");
        let (st, _, body) = self
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/disable"),
                Some(TOKEN),
                &[("if-match", &if_match), ("idempotency-key", key)],
                Some(json!({})),
            ))
            .await;
        let op = assert_ok(st, &body, StatusCode::ACCEPTED);
        assert_eq!(op["state"], "succeeded");
        op
    }
}

async fn create_member(app: &axum::Router, login: &str) -> String {
    let created = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/users",
            Some(TOKEN),
            &[],
            Some(json!({ "login": login, "password": "member-pass-123" })),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let logged = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/auth/login",
            None,
            &[],
            Some(json!({ "username": login, "password": "member-pass-123" })),
        ))
        .await
        .unwrap();
    assert_eq!(logged.status(), StatusCode::OK);
    let bytes = to_bytes(logged.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["data"]["token"].as_str().unwrap().to_string()
}

pub fn req_json(
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(tok) = token {
        builder = builder.header("authorization", format!("Bearer {tok}"));
    }
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

pub fn req_multipart(
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    boundary: &str,
    parts: &[(&str, Option<&str>, &[u8])],
) -> Request<Body> {
    let mut payload = Vec::new();
    for (name, filename, data) in parts {
        payload.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match filename {
            Some(f) => payload.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => payload.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        payload.extend_from_slice(data);
        payload.extend_from_slice(b"\r\n");
    }
    payload.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let mut builder = Request::builder().method("POST").uri(uri).header(
        "content-type",
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(tok) = token {
        builder = builder.header("authorization", format!("Bearer {tok}"));
    }
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    builder.body(Body::from(payload)).unwrap()
}

pub fn assert_ok(status: StatusCode, body: &Value, expected: StatusCode) -> Value {
    assert_eq!(status, expected, "unexpected status, body={body}");
    assert_eq!(body["ok"], true, "expected ok=true, body={body}");
    body["data"].clone()
}

pub fn assert_paged(status: StatusCode, body: &Value) -> (Vec<Value>, Option<String>) {
    assert_eq!(status, StatusCode::OK, "unexpected status, body={body}");
    assert_eq!(body["ok"], true, "expected ok=true, body={body}");
    assert!(
        body.get("next_cursor").is_some(),
        "paged envelope must have top-level next_cursor: {body}"
    );
    let items = body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("paged data must be array: {body}"))
        .clone();
    let next = body["next_cursor"].as_str().map(str::to_string);
    (items, next)
}

pub fn assert_err(status: StatusCode, body: &Value, expected: StatusCode, code: &str) -> Value {
    assert_eq!(status, expected, "unexpected status, body={body}");
    assert_eq!(body["ok"], false, "expected ok=false, body={body}");
    let err = &body["error"];
    assert_eq!(
        err["code"].as_str(),
        Some(code),
        "unexpected error code, body={body}"
    );
    assert!(err["message"].as_str().is_some(), "missing message: {body}");
    err.clone()
}

pub fn etag(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::ETAG)
        .unwrap_or_else(|| panic!("missing ETag header: {headers:?}"))
        .to_str()
        .unwrap()
        .to_string()
}

pub fn location(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::LOCATION)
        .unwrap_or_else(|| panic!("missing Location header: {headers:?}"))
        .to_str()
        .unwrap()
        .to_string()
}

pub fn build_21m_pkg(id: &str) -> Vec<u8> {
    let blob_21m = vec![0x42u8; 21 * 1024 * 1024];
    let manifest = pkg::task_manifest_with_options(id, "1.0.0", 1, false, &["kv"]);
    let cfg_schema = pkg::sample_schema(false, None);
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        ("components/worker.wasm", pkg::task_wasm()),
        ("assets/large.bin", &blob_21m),
    ])
}
