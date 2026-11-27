#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{MaintenanceVia, Site, SiteId};
use downloader::{Downloader, MemoryDownloader};
use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};
use plugin_http_support::{ADMIN_PASS, TOKEN, req_json};
use serde_json::json;
use tower::ServiceExt;

struct NoopFetcher;
impl Fetcher for NoopFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

type RecordingServer = (
    SocketAddr,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicUsize>,
    std::thread::JoinHandle<()>,
);

fn spawn_recording_server(responses: Vec<String>) -> RecordingServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let reqs_srv = Arc::clone(&reqs);
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        for resp in responses {
            loop {
                if Instant::now() > deadline {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                        let mut full = Vec::new();
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = stream.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            full.extend_from_slice(&buf[..n]);
                            if let Some(hdr_end) = full.windows(4).position(|w| w == b"\r\n\r\n") {
                                let hdr =
                                    String::from_utf8_lossy(&full[..hdr_end]).to_ascii_lowercase();
                                let cl = hdr
                                    .lines()
                                    .find_map(|l| l.strip_prefix("content-length:"))
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                                    .unwrap_or(0);
                                if full.len() >= hdr_end + 4 + cl {
                                    break;
                                }
                            }
                        }
                        hits_srv.fetch_add(1, Ordering::SeqCst);
                        reqs_srv
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&full).into_owned());
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        }
    });
    (addr, reqs, hits, handle)
}

fn make_site(url: String, profile_id: &str, cookie: Option<&str>, api_key: Option<&str>) -> Site {
    Site {
        id: SiteId::new(),
        name: "R12-Site".into(),
        url,
        profile_id: profile_id.into(),
        cookie: cookie.map(Into::into),
        api_key: api_key.map(Into::into),
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

#[tokio::test]
async fn api_check_in_strips_x_api_key_on_cross_origin_redirect_and_blocks_ssrf() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("dl"))) as Arc<dyn Downloader>;
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());

    // 1. Cross-origin 307 redirect: X-API-Key must NOT be forwarded to origin B.
    let (addr_b, reqs_b, hits_b, srv_b) = spawn_recording_server(vec![
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{\"code\":0}".into(),
    ]);
    let (addr_a, reqs_a, _, srv_a) = spawn_recording_server(vec![format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{addr_b}/collect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )]);
    let policy = net_guard::TargetPolicy::production()
        .with_test_loopback_socket(addr_a)
        .with_test_loopback_socket(addr_b);
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
    .with_plugin_http_policy_override(policy);
    let site_a = make_site(
        format!("http://{addr_a}"),
        "hdarea",
        None,
        Some("TOPSECRET_API_KEY_777"),
    );
    state.store().insert_site(&site_a).unwrap();
    let app = router(state);

    let resp = app
        .oneshot(req_json(
            "POST",
            &format!("/api/v1/sites/{}/check-in", site_a.id),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let resp_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "resp={}",
        String::from_utf8_lossy(&resp_bytes)
    );
    srv_a.join().unwrap();
    srv_b.join().unwrap();
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
    let first_req = reqs_a.lock().unwrap()[0].to_ascii_lowercase();
    let second_req = reqs_b.lock().unwrap()[0].to_ascii_lowercase();
    assert!(first_req.contains("x-api-key: topsecret_api_key_777"));
    assert!(
        !second_req.contains("topsecret_api_key_777") && !second_req.contains("x-api-key"),
        "cross-origin redirect leaked X-API-Key: {second_req}"
    );
}

#[tokio::test]
async fn check_in_blocks_redirect_to_self_listen_and_sanitizes_error_output() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("dl"))) as Arc<dyn Downloader>;
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());

    let (self_addr, _, self_hits, _self_srv) = spawn_recording_server(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into(),
    ]);
    let (att_addr, _, _, att_srv) = spawn_recording_server(vec![format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{self_addr}/api/v1/health\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )]);
    let err_json = r#"{"code":1,"message":"upstream https://pt.example/rss?passkey=abcdef0123456789abcdef0123456789 failed"}"#;
    let (api_addr, _, _, api_srv) = spawn_recording_server(vec![format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{err_json}",
        err_json.len()
    )]);

    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
    .with_self_listen(self_addr);
    let app = router(state.clone());
    let create_resp = app
        .clone()
        .oneshot(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "NexusR12",
                "url": format!("http://{att_addr}/"),
                "profile_id": "pterclub",
                "cookie": "uid=1; pass=init",
                "maintenance_via": "http",
                "timezone": "UTC",
                "enabled": true,
            })),
        ))
        .await
        .unwrap();
    let create_bytes = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let nexus_id: SiteId =
        serde_json::from_slice::<serde_json::Value>(&create_bytes).unwrap()["data"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
    let hdarea = make_site(format!("http://{api_addr}"), "hdarea", None, Some("k-123"));
    state.store().insert_site(&hdarea).unwrap();

    let r_att = app
        .clone()
        .oneshot(req_json(
            "POST",
            &format!("/api/v1/sites/{nexus_id}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(r_att.status(), StatusCode::BAD_REQUEST);
    att_srv.join().unwrap();
    assert_eq!(
        self_hits.load(Ordering::SeqCst),
        0,
        "AttendanceHttp must never connect to self_listen on redirect"
    );

    let r_api = app
        .oneshot(req_json(
            "POST",
            &format!("/api/v1/sites/{}/check-in", hdarea.id),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(r_api.status(), StatusCode::BAD_REQUEST);
    api_srv.join().unwrap();
    let bytes = axum::body::to_bytes(r_api.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&bytes);
    assert!(
        !body_str.contains("abcdef0123456789abcdef0123456789"),
        "check-in failure response leaked passkey: {body_str}"
    );
}
