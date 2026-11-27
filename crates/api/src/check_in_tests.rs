use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};

use hooks::{HttpPost, PluginError};

use super::AttendanceHttp;

fn loopback_http(addr: SocketAddr) -> AttendanceHttp {
    AttendanceHttp {
        policy: net_guard::TargetPolicy::production().with_test_loopback_socket(addr),
    }
}

#[test]
fn attendance_http_post_302_to_dead_port_returns_unknown_outcome_while_hop0_returns_fetch() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        drop(listener);
        let _ = stream.write_all(
            b"HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let _ = stream.flush();
        drop(stream);
    });

    let err_redirect = loopback_http(addr)
        .post(&format!("http://{addr}/attendance.php"), "a=1", None, None)
        .unwrap_err();
    srv.join().unwrap();
    assert!(
        matches!(err_redirect, PluginError::UnknownOutcome(_)),
        "POST -> 302 -> ConnectionRefused must return UnknownOutcome, got {err_redirect:?}"
    );

    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let err_hop0 = loopback_http(dead_addr)
        .post(
            &format!("http://{dead_addr}/attendance.php"),
            "a=1",
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err_hop0, PluginError::Fetch(_)),
        "Hop-0 POST -> ConnectionRefused must return Fetch, got {err_hop0:?}"
    );
}

fn spawn_single_response(response: Vec<u8>) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(&response);
        let _ = stream.flush();
        drop(stream);
    });
    (addr, handle)
}

#[test]
fn attendance_http_hop0_post_500_returns_unknown_outcome() {
    let (addr, srv) = spawn_single_response(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops"
            .to_vec(),
    );
    let err = loopback_http(addr)
        .post(&format!("http://{addr}/attendance.php"), "a=1", None, None)
        .unwrap_err();
    srv.join().unwrap();
    assert!(
        matches!(err, PluginError::UnknownOutcome(_)),
        "hop-0 POST -> HTTP 500 must return UnknownOutcome, got {err:?}"
    );
}

#[test]
fn attendance_http_hop0_post_403_challenge_returns_fetch_while_plain_403_returns_unknown_outcome() {
    let challenge_body = r#"<div id="cf-challenge-running">Checking your browser</div>"#;
    let challenge_resp = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{challenge_body}",
        challenge_body.len()
    );
    let (addr_c, srv_c) = spawn_single_response(challenge_resp.into_bytes());
    let err_c = loopback_http(addr_c)
        .post(
            &format!("http://{addr_c}/attendance.php"),
            "a=1",
            None,
            None,
        )
        .unwrap_err();
    srv_c.join().unwrap();
    assert!(
        matches!(err_c, PluginError::Fetch(ref msg) if msg.contains("验证码") || msg.contains("登录表单")),
        "hop-0 POST -> 403 CF challenge must return challenge Fetch, got {err_c:?}"
    );

    let (addr_p, srv_p) = spawn_single_response(
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\nConnection: close\r\n\r\nForbidden"
            .to_vec(),
    );
    let err_p = loopback_http(addr_p)
        .post(
            &format!("http://{addr_p}/attendance.php"),
            "a=1",
            None,
            None,
        )
        .unwrap_err();
    srv_p.join().unwrap();
    assert!(
        matches!(err_p, PluginError::UnknownOutcome(_)),
        "hop-0 POST -> plain 403 must return UnknownOutcome, got {err_p:?}"
    );
}

#[test]
fn attendance_http_get_500_with_logout_link_returns_fetch_and_503_challenge_surfaces_challenge() {
    let body_500 = r#"<html><a href="logout.php">Logout</a><p>Server Error</p></html>"#;
    let resp_500 = format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_500}",
        body_500.len()
    );
    let (addr_500, srv_500) = spawn_single_response(resp_500.into_bytes());
    let err_500 = loopback_http(addr_500)
        .get(&format!("http://{addr_500}/index.php"), Some("uid=1"), None)
        .unwrap_err();
    srv_500.join().unwrap();
    assert!(
        matches!(err_500, PluginError::Fetch(_)),
        "GET -> 500 with logout.php must return Fetch (never Ok), got {err_500:?}"
    );

    let body_503 = "<html><title>Just a moment...</title></html>";
    let resp_503 = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_503}",
        body_503.len()
    );
    let (addr_503, srv_503) = spawn_single_response(resp_503.into_bytes());
    let err_503 = loopback_http(addr_503)
        .get(&format!("http://{addr_503}/index.php"), Some("uid=1"), None)
        .unwrap_err();
    srv_503.join().unwrap();
    assert!(
        matches!(err_503, PluginError::Fetch(ref msg) if msg.contains("验证码") || msg.contains("登录表单")),
        "GET -> 503 challenge must surface challenge Fetch, got {err_503:?}"
    );
}

#[test]
fn resolve_attendance_redirect_https_downgrade_respects_mutating_sent() {
    let current = net_guard::normalize_url("https://site.example/attendance.php", true).unwrap();
    let policy = net_guard::TargetPolicy::production();
    let mut headers = ureq::http::HeaderMap::new();
    headers.insert(
        "location",
        ureq::http::HeaderValue::from_static("http://site.example/done"),
    );

    let err_post =
        super::resolve_guarded_redirect(&current, &policy, &headers, 302, "POST", 0, true)
            .unwrap_err();
    assert!(
        matches!(err_post, PluginError::UnknownOutcome(_)),
        "https -> http downgrade with mutating_sent=true must return UnknownOutcome, got {err_post:?}"
    );

    let err_get =
        super::resolve_guarded_redirect(&current, &policy, &headers, 302, "GET", 0, false)
            .unwrap_err();
    assert!(
        matches!(err_get, PluginError::Fetch(_)),
        "https -> http downgrade with mutating_sent=false must return Fetch, got {err_get:?}"
    );
}

#[test]
fn attendance_http_post_302_to_get_500_returns_unknown_outcome() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = std::thread::spawn(move || {
        for idx in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let resp: &[u8] = if idx == 0 {
                b"HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 3\r\nConnection: close\r\n\r\nerr"
            };
            let _ = stream.write_all(resp);
            let _ = stream.flush();
        }
    });
    let err = loopback_http(addr)
        .post(&format!("http://{addr}/attendance.php"), "a=1", None, None)
        .unwrap_err();
    srv.join().unwrap();
    assert!(
        matches!(err, PluginError::UnknownOutcome(_)),
        "POST -> 302 -> GET 500 must return UnknownOutcome, got {err:?}"
    );
}
