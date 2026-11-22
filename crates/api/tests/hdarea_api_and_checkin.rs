#[path = "management/common.rs"]
mod common;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, HttpFetcher, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

fn build_app(root: &std::path::Path) -> (axum::Router, ApiState) {
    let state = ApiState::new(
        api::Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(HttpFetcher),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .unwrap();
    (router(state.clone()), state)
}

struct StubServer {
    address: SocketAddr,
    captured: Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start(responses: Vec<(u16, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_bg = captured.clone();
        let handle = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let raw = read_http_request(&mut stream);
                if raw.starts_with("GET /__stop ") || raw.is_empty() {
                    break;
                }
                captured_bg.lock().push(raw);
                let (status, body) = queue.lock().pop_front().unwrap_or((500, "{}"));
                let reason = if status == 200 { "OK" } else { "Error" };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            address,
            captured,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<String> {
        self.captured.lock().clone()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            if let Ok(mut stream) =
                TcpStream::connect_timeout(&self.address, Duration::from_millis(100))
            {
                let _ = stream.write_all(b"GET /__stop HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
            }
            let _ = handle.join();
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    let mut header_end = None;
    loop {
        let Ok(n) = stream.read(&mut chunk) else {
            break;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = Some(pos + 4);
            break;
        }
    }
    if let Some(end) = header_end {
        let headers = String::from_utf8_lossy(&buf[..end]);
        let content_len: usize = headers
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < end + content_len {
            let Ok(n) = stream.read(&mut chunk) else {
                break;
            };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

const HDAREA_VERIFY_OK: &str = r#"{
  "success": true,
  "code": 0,
  "http_code": 200,
  "message": "OK",
  "data": {
    "items": [
      {
        "id": 469752,
        "name": "Knowing Bros S01E552 2015 1080p Viu WEB-DL H264 AAC-ADWeb",
        "small_descr": "认识的哥哥",
        "category_id": 403,
        "category_name": "TV Shows",
        "size": 2465419469,
        "seeders": 8,
        "leechers": 7,
        "times_completed": 7,
        "added": "2026-11-22 14:57:37",
        "sp_state": 2,
        "download_volume_factor": 0.0,
        "is_free": true,
        "download_url": "https://hdarea.club/download.php?id=469752&dl_token=tok"
      }
    ]
  }
}"#;

#[tokio::test]
async fn sites_catalog_exposes_hdarea_hybrid_auth_and_check_in_support() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = build_app(root.path());
    let res = app
        .oneshot(common::request(
            "GET",
            "/api/v1/sites/catalog",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rows = common::json_data(res).await;
    let items = rows.as_array().unwrap();

    let hdarea = items.iter().find(|r| r["profile_id"] == "hdarea").unwrap();
    assert_eq!(hdarea["auth_types"], json!(["apikey", "cookie"]));
    assert_eq!(hdarea["supports_check_in"], json!(true));

    let mteam = items.iter().find(|r| r["profile_id"] == "mteam").unwrap();
    assert_eq!(mteam["auth_types"], json!(["apikey"]));
    assert_eq!(mteam["supports_check_in"], json!(false));

    let pter = items
        .iter()
        .find(|r| r["profile_id"] == "pterclub")
        .unwrap();
    assert_eq!(pter["auth_types"], json!(["cookie"]));
    assert_eq!(pter["supports_check_in"], json!(true));
}

#[tokio::test]
async fn hdarea_verify_and_check_in_use_rest_v1_api_key_contract() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = build_app(root.path());
    let stub = StubServer::start(vec![
        (200, HDAREA_VERIFY_OK),
        (
            200,
            r#"{"success":true,"code":0,"message":"签到成功，获得 50 魔力值"}"#,
        ),
        (200, r#"{"success":false,"code":1,"message":"今日已签到"}"#),
    ]);

    let created = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "HDArea",
                "url": stub.base_url(),
                "profile_id": "hdarea",
                "api_key": "0123456789abcdef0123456789abcdef",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let site_id = common::json_data(created).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let verify_res = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/sites/{site_id}/verify"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(verify_res.status(), StatusCode::OK);
    let verify_data = common::json_data(verify_res).await;
    assert_eq!(verify_data["ok"], true);
    assert_eq!(verify_data["torrent_count"], 1);

    for _ in 0..2 {
        let checkin_res = app
            .clone()
            .oneshot(common::request(
                "POST",
                &format!("/api/v1/sites/{site_id}/check-in"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(checkin_res.status(), StatusCode::OK);
        assert_eq!(common::json_data(checkin_res).await["ok"], true);
    }

    let login_res = app
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/sites/{site_id}/login"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::json_error(login_res).await["code"],
        "site.maintenance_unsupported"
    );

    let reqs = stub.requests();
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs[0].starts_with("GET /api/v1/torrents?incldead=1&per_page=100&page=0 "),
        "{}",
        reqs[0]
    );
    assert!(
        reqs[0]
            .to_ascii_lowercase()
            .contains("x-api-key: 0123456789abcdef0123456789abcdef")
    );
    assert!(
        reqs[1].starts_with("POST /api/v1/user/signin "),
        "{}",
        reqs[1]
    );
    assert!(
        reqs[1]
            .to_ascii_lowercase()
            .contains("x-api-key: 0123456789abcdef0123456789abcdef")
    );
    assert!(reqs[1].ends_with("{}"), "{}", reqs[1]);
}

#[tokio::test]
async fn hdarea_scheduled_check_in_runs_and_invalid_key_or_placeholder_fails() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = build_app(root.path());
    let stub = StubServer::start(vec![
        (
            401,
            r#"{"success":false,"code":401,"message":"Invalid API key"}"#,
        ),
        (200, r#"{"success":true,"code":0,"message":"OK"}"#),
    ]);

    let created = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "HDArea",
                "url": stub.base_url(),
                "profile_id": "hdarea",
                "api_key": "0123456789abcdef0123456789abcdef",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    let site_id = common::json_data(created).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 1. 401 failure surfaces server message
    let bad_res = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/sites/{site_id}/check-in"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(bad_res.status(), StatusCode::BAD_REQUEST);
    let err = common::json_error(bad_res).await;
    assert_eq!(err["code"], "site.check_in_failed");
    assert!(err["message"].as_str().unwrap().contains("Invalid API key"));

    // 2. Scheduled check_in job tick hits /api/v1/user/signin and succeeds
    let mut ran = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let tick = app
            .clone()
            .oneshot(common::request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let data = common::json_data(tick).await;
        if data["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "check_in")
        {
            ran = true;
            break;
        }
    }
    assert!(ran, "scheduled check_in job must run");
    assert_eq!(stub.requests().len(), 2);

    let jobs_db = rusqlite::Connection::open(root.path().join("data/jobs.db")).unwrap();
    let job_err: Option<String> = jobs_db
        .query_row(
            "SELECT error FROM jobs WHERE kind = 'check_in' ORDER BY run_after DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(job_err, None, "scheduled API check-in must succeed");
}

#[tokio::test]
async fn hdarea_api_check_in_rejects_placeholder_urls() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = build_app(root.path());
    let ph_created = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "HDAreaPlaceholder",
                "url": "https://hdarea.example/",
                "profile_id": "hdarea",
                "api_key": "0123456789abcdef0123456789abcdef",
                "enabled": false
            }),
        ))
        .await
        .unwrap();
    let ph_id = common::json_data(ph_created).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ph_res = app
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/sites/{ph_id}/check-in"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(ph_res.status(), StatusCode::BAD_REQUEST);
    assert!(
        common::json_error(ph_res).await["message"]
            .as_str()
            .unwrap()
            .contains("占位地址")
    );
}

#[tokio::test]
async fn patch_site_switches_between_cookie_and_apikey_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let (app, state) = build_app(root.path());

    let created = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "HDArea",
                "url": "",
                "profile_id": "hdarea",
                "cookie": "c_secure_uid=1",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let view = common::json_data(created).await;
    assert_eq!(view["url"], "https://hdarea.club");
    assert_eq!(view["auth_type"], "cookie");
    let id_str = view["id"].as_str().unwrap().to_string();
    let site_id: domain::SiteId = id_str.parse().unwrap();

    // Switch cookie -> apikey (clears cookie)
    let to_api = app
        .clone()
        .oneshot(common::request(
            "PATCH",
            &format!("/api/v1/sites/{id_str}"),
            Some("management-secret"),
            json!({
                "api_key": "0123456789abcdef0123456789abcdef",
                "cookie": "",
                "maintenance_via": "http"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(common::json_data(to_api).await["auth_type"], "apikey");
    let saved = state.store().get_site(site_id).unwrap().unwrap();
    assert_eq!(saved.cookie, None);
    assert_eq!(
        saved.api_key.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );

    // Update URL while omitting api_key -> preserves saved api_key
    let keep_api = app
        .clone()
        .oneshot(common::request(
            "PATCH",
            &format!("/api/v1/sites/{id_str}"),
            Some("management-secret"),
            json!({ "url": "https://www.hdarea.co", "cookie": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(common::json_data(keep_api).await["auth_type"], "apikey");
    let saved2 = state.store().get_site(site_id).unwrap().unwrap();
    assert_eq!(
        saved2.api_key.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );

    // Switch apikey -> cookie (clears api_key)
    let to_cookie = app
        .oneshot(common::request(
            "PATCH",
            &format!("/api/v1/sites/{id_str}"),
            Some("management-secret"),
            json!({ "cookie": "c_secure_uid=2", "api_key": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(common::json_data(to_cookie).await["auth_type"], "cookie");
    let saved3 = state.store().get_site(site_id).unwrap().unwrap();
    assert_eq!(saved3.api_key, None);
    assert_eq!(saved3.cookie.as_deref(), Some("c_secure_uid=2"));
}
