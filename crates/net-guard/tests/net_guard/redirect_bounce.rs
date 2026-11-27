use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use net_guard::{
    CredentialSlot, GuardedMethod, GuardedRequest, NetGuardClient, Origin, Scheme, TargetPolicy,
};

use super::support::TestServer;

fn read_raw_request(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn spawn_bound_server(
    listener: TcpListener,
    responses: Vec<Vec<u8>>,
) -> (Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);
    let handle = thread::spawn(move || {
        for resp in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_raw_request(&mut stream);
            captured_clone.lock().unwrap().push(req);
            let _ = stream.write_all(&resp);
            let _ = stream.flush();
        }
    });
    (captured, handle)
}

#[test]
fn cross_origin_redirect_bounce_drops_credentials_and_blocks_cookie_capture() {
    let listener_a = TcpListener::bind("127.0.0.1:0").unwrap();
    let listener_b = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr_a: SocketAddr = listener_a.local_addr().unwrap();
    let addr_b: SocketAddr = listener_b.local_addr().unwrap();

    let (reqs_a, handle_a) = spawn_bound_server(
        listener_a,
        vec![
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/bounce\r\nSet-Cookie: hop0=ok\r\nContent-Length: 0\r\n\r\n",
                addr_b.port()
            )
            .into_bytes(),
            b"HTTP/1.1 200 OK\r\nSet-Cookie: bounced=evil\r\nContent-Length: 7\r\n\r\nbounced"
                .to_vec(),
        ],
    );
    let (reqs_b, handle_b) = spawn_bound_server(
        listener_b,
        vec![
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/back\r\nSet-Cookie: hop1=foreign\r\nContent-Length: 0\r\n\r\n",
                addr_a.port()
            )
            .into_bytes(),
        ],
    );

    let origin_a = Origin::new(Scheme::Http, "127.0.0.1", Some(addr_a.port()));
    let creds = CredentialSlot::new(
        "site-a",
        origin_a,
        "epoch-1",
        vec![
            ("Cookie".into(), "sid=secret-cookie".into()),
            ("Authorization".into(), "Bearer secret-token".into()),
        ],
    );
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr_a)
        .with_test_loopback_socket(addr_b);

    let resp = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/start", addr_a.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            Some(&creds),
        )
        .unwrap();

    handle_a.join().unwrap();
    handle_b.join().unwrap();

    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"bounced");
    assert_eq!(resp.captured_set_cookies, vec!["hop0=ok".to_string()]);

    let a = reqs_a.lock().unwrap();
    let b = reqs_b.lock().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 1);

    let hop0_lower = a[0].to_ascii_lowercase();
    assert!(hop0_lower.contains("cookie: sid=secret-cookie"));
    assert!(hop0_lower.contains("authorization: bearer secret-token"));

    for (label, raw) in [("hop1", &b[0]), ("hop2", &a[1])] {
        let lower = raw.to_ascii_lowercase();
        assert!(
            !lower.contains("secret-cookie") && !lower.contains("secret-token"),
            "{label} must not receive credentials:\n{raw}"
        );
    }
}

#[test]
fn pure_same_origin_redirect_preserves_credentials_and_captures_cookies() {
    let mut server_a = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nLocation: /step2\r\nSet-Cookie: s1=one\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nSet-Cookie: s2=two\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let addr_a = server_a.addr;
    let origin_a = Origin::new(Scheme::Http, "127.0.0.1", Some(addr_a.port()));
    let creds = CredentialSlot::new(
        "site-a",
        origin_a,
        "epoch-1",
        vec![
            ("Cookie".into(), "sid=secret-cookie".into()),
            ("Authorization".into(), "Bearer secret-token".into()),
        ],
    );
    let policy = TargetPolicy::production().with_test_loopback_socket(addr_a);

    let resp = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/step1", addr_a.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            Some(&creds),
        )
        .unwrap();

    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"ok");
    assert_eq!(
        resp.captured_set_cookies,
        vec!["s1=one".to_string(), "s2=two".to_string()]
    );

    let reqs = server_a.take_requests();
    assert_eq!(reqs.len(), 2);
    for (idx, req) in reqs.iter().enumerate() {
        let lower = req.to_ascii_lowercase();
        assert!(
            lower.contains("cookie: sid=secret-cookie")
                && lower.contains("authorization: bearer secret-token"),
            "same-origin hop {idx} must retain credentials:\n{req}"
        );
    }
}
