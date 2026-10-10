#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use api::{ApiState, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

const LOGIN_FORM: &str =
    "<html><form action='takelogin.php'><input name='password'/></form></html>";

fn app(root: &std::path::Path) -> (axum::Router, ApiState) {
    let overlay = root.join("profiles");
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(overlay.join("pterclub.yaml"), "id: pterclub\nframework: nexusphp\nlogin_success_css: '#session-confirmed'\nsearch:\n  path: /torrents.php\n  query_param: search\n").unwrap();
    let state = ApiState::new(
        api::Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(Some(&overlay)).unwrap(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    (router(state.clone()), state)
}

struct FakeServer {
    address: std::net::SocketAddr,
    job: Option<std::thread::JoinHandle<String>>,
}

impl FakeServer {
    fn unblock(&self) {
        if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
            &self.address,
            std::time::Duration::from_millis(100),
        ) {
            let _ = stream.write_all(b"GET /__cancel HTTP/1.1\r\nHost: fixture\r\n\r\n");
        }
    }

    fn join(mut self) -> std::thread::Result<String> {
        self.unblock();
        self.job.take().unwrap().join()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        if self.job.is_some() {
            self.unblock();
            let _ = self.job.take().unwrap().join();
        }
    }
}

fn fake_http(body: &'static str, headers: &'static str) -> (String, FakeServer) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/");
    let job = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                break;
            }
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
        .unwrap();
        String::from_utf8(request).unwrap()
    });
    (
        url,
        FakeServer {
            address,
            job: Some(job),
        },
    )
}

async fn create(app: &axum::Router, url: &str, cookie: Value) -> domain::SiteId {
    let mut payload = common::site_payload();
    payload["url"] = json!(url);
    payload["profile_id"] = json!("pterclub");
    payload["cookie"] = cookie;
    let response = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            payload,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    common::json_data(response).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn action(app: &axum::Router, id: domain::SiteId, action: &str) -> axum::response::Response {
    app.clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/sites/{id}/{action}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn actual_http_login_and_check_in_reject_login_forms_without_mutating_credentials() {
    for operation in ["login", "check-in"] {
        let root = tempfile::tempdir().unwrap();
        let (app, state) = app(root.path());
        let (url, server) = fake_http(LOGIN_FORM, "Set-Cookie: uid=unverified\r\n");
        let id = create(&app, &url, json!("uid=old; pass=secret")).await;
        let response = action(&app, id, operation).await;
        server.join().unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{operation}");
        assert_eq!(common::json_body(response).await["ok"], false);
        assert_eq!(
            state
                .store()
                .lock()
                .get_site(id)
                .unwrap()
                .unwrap()
                .cookie
                .as_deref(),
            Some("uid=old; pass=secret")
        );
    }
}

#[tokio::test]
async fn actual_http_check_in_confirms_success_and_preserves_all_rotated_cookie_headers() {
    let root = tempfile::tempdir().unwrap();
    let (app, state) = app(root.path());
    let (url, server) = fake_http(
        "<html>您今天已经签到过了</html>",
        "Set-Cookie: session=fresh; Path=/\r\nSet-Cookie: token=fresh; Path=/\r\n",
    );
    let id = create(&app, &url, json!("uid=old; pass=secret")).await;
    let response = action(&app, id, "check-in").await;
    let request = server.join().unwrap();
    assert!(request.starts_with("POST /attendance.php"));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::json_data(response).await["ok"], true);
    let cookie = state
        .store()
        .lock()
        .get_site(id)
        .unwrap()
        .unwrap()
        .cookie
        .unwrap();
    for expected in ["uid=old", "pass=secret", "session=fresh", "token=fresh"] {
        assert!(cookie.contains(expected), "{cookie} lacks {expected}");
    }
}

#[tokio::test]
async fn actual_http_login_verifies_existing_session_using_profile_success_selector() {
    let root = tempfile::tempdir().unwrap();
    let (app, state) = app(root.path());
    let (url, server) = fake_http("<html><div id='session-confirmed'></div></html>", "");
    let id = create(&app, &url, json!("uid=old; pass=secret")).await;
    let response = action(&app, id, "login").await;
    let request = server.join().unwrap();
    assert!(
        request.starts_with("GET /index.php"),
        "must verify, not submit an empty login: {request}"
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state
            .store()
            .lock()
            .get_site(id)
            .unwrap()
            .unwrap()
            .cookie
            .as_deref(),
        Some("uid=old; pass=secret")
    );
}

#[tokio::test]
async fn scheduled_job_retries_when_actual_http_returns_a_login_form() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = app(root.path());
    let (url, server) = fake_http(LOGIN_FORM, "");
    create(&app, &url, json!("uid=old; pass=secret")).await;
    let mut ran = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(common::request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let body = common::json_data(response).await;
        if body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "check_in")
        {
            ran = true;
            break;
        }
    }
    assert!(ran, "Check-in Job did not run");
    server.join().unwrap();
    let jobs = rusqlite::Connection::open(root.path().join("data/jobs.db")).unwrap();
    let (status, error): (String, String) = jobs.query_row("SELECT status, error FROM jobs WHERE kind = 'check_in' ORDER BY run_after DESC LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(status, "queued");
    assert!(error.contains("登录表单"), "{error}");
}

#[tokio::test]
async fn missing_login_credentials_return_actionable_error_without_http_submission() {
    let root = tempfile::tempdir().unwrap();
    let (app, _) = app(root.path());
    let id = create(&app, "http://127.0.0.1:1/", Value::Null).await;
    let response = action(&app, id, "login").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = common::json_error(response).await;
    assert!(
        error["message"].as_str().unwrap().contains("Cookie"),
        "{error}"
    );
}
