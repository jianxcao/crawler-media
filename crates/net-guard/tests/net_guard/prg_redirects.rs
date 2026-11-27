use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use net_guard::{
    DnsResolver, GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, TargetPolicy,
};

use super::support::{FakeResolver, TestServer};

#[derive(Debug)]
struct TimeoutOnHostResolver {
    allowed_host: String,
    allowed_ip: IpAddr,
}

impl DnsResolver for TimeoutOnHostResolver {
    fn resolve(
        &self,
        host: &str,
        _port: u16,
        _timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError> {
        if host == self.allowed_host {
            Ok(vec![self.allowed_ip])
        } else {
            Err(NetGuardError::Timeout("simulated hop1 dns timeout".into()))
        }
    }
}

fn spawn_single_302_and_close_listener(
    location: String,
) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        // Drop the listening socket before replying so the redirect target immediately refuses connections.
        drop(listener);
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        drop(stream);
    });
    (addr, handle)
}

#[test]
fn post_redirect_get_hop1_connection_refused_returns_post_send_unknown() {
    let (addr, handle) = spawn_single_302_and_close_listener("/done".into());
    let client = NetGuardClient::new();
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/attendance.php", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    handle.join().unwrap();
    assert!(
        matches!(err, NetGuardError::PostSendUnknown(_)),
        "POST -> 302 -> ConnectionRefused must return PostSendUnknown, got {err:?}"
    );
}

#[test]
fn post_redirect_get_hop1_dns_failure_and_timeout_return_post_send_unknown() {
    let server = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nLocation: http://hop1-missing.example:8080/done\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://hop1-timeout.example:8080/done\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    ]);
    let addr = server.addr;
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);

    let dns_fail_resolver = Arc::new(FakeResolver::new(HashMap::from([(
        "hop0.example".into(),
        vec![vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]],
    )])));
    let err_dns = NetGuardClient::with_resolver(dns_fail_resolver)
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://hop0.example:{}/start", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err_dns, NetGuardError::PostSendUnknown(_)),
        "POST -> 302 -> hop1 DNS Transport must return PostSendUnknown, got {err_dns:?}"
    );

    let dns_timeout_resolver = Arc::new(TimeoutOnHostResolver {
        allowed_host: "hop0.example".into(),
        allowed_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
    });
    let err_timeout = NetGuardClient::with_resolver(dns_timeout_resolver)
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://hop0.example:{}/start", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err_timeout, NetGuardError::PostSendUnknown(_)),
        "POST -> 302 -> hop1 DNS Timeout must return PostSendUnknown, got {err_timeout:?}"
    );
}

#[test]
fn post_redirect_get_hop1_tls_error_returns_post_send_unknown() {
    let tls_peer = TestServer::spawn(vec![b"NOT_TLS_DATA\r\n\r\n".to_vec()]);
    let tls_addr = tls_peer.addr;
    let hop0_srv = TestServer::spawn(vec![
        format!(
            "HTTP/1.1 302 Found\r\nLocation: https://tls.example:{}/done\r\nContent-Length: 0\r\n\r\n",
            tls_addr.port()
        )
        .into_bytes(),
    ]);
    let hop0_addr = hop0_srv.addr;
    let resolver = Arc::new(FakeResolver::new(HashMap::from([
        (
            "hop0.example".into(),
            vec![vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]],
        ),
        (
            "tls.example".into(),
            vec![vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]],
        ),
    ])));
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(hop0_addr)
        .with_test_loopback_socket(tls_addr);
    let err = NetGuardClient::with_resolver(resolver)
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://hop0.example:{}/start", hop0_addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::PostSendUnknown(_)),
        "POST -> 302 -> TLS failure must return PostSendUnknown, got {err:?}"
    );
}

#[test]
fn post_redirect_missing_location_returns_post_send_unknown() {
    let server = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    let addr = server.addr;
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let err = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/submit", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::PostSendUnknown(_)),
        "POST -> 302 missing Location must return PostSendUnknown, got {err:?}"
    );
}

#[test]
fn post_redirect_to_blocked_self_preserves_blocked_address() {
    let blocked_srv = TestServer::spawn(vec![]);
    let blocked_addr = blocked_srv.addr;
    let server = TestServer::spawn(vec![
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/admin\r\nContent-Length: 0\r\n\r\n",
            blocked_addr.port()
        )
        .into_bytes(),
    ]);
    let addr = server.addr;
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_blocked_self(blocked_addr);
    let err = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/submit", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            NetGuardError::PostSendBlocked(ref inner)
                if matches!(inner.as_ref(), NetGuardError::BlockedAddress(_))
        ),
        "POST -> 302 -> blocked_self must return PostSendBlocked(BlockedAddress), got {err:?}"
    );
    assert!(err.is_post_send_unknown());
    assert!(matches!(err.cause(), NetGuardError::BlockedAddress(_)));
}

#[test]
fn post_redirect_dns_blocked_hop1_and_get_redirect_contrast() {
    let server = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nLocation: http://private.example:8080/admin\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://private.example:8080/admin\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    ]);
    let addr = server.addr;
    let resolver = Arc::new(FakeResolver::new(HashMap::from([
        (
            "hop0.example".into(),
            vec![vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]],
        ),
        (
            "private.example".into(),
            vec![vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]],
        ),
    ])));
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let client = NetGuardClient::with_resolver(resolver);

    let err_private = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://hop0.example:{}/submit", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(
        err_private,
        NetGuardError::PostSendBlocked(ref inner)
            if matches!(inner.as_ref(), NetGuardError::BlockedAddress(_))
    ));
    assert!(err_private.is_post_send_unknown());

    let err_meta = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://hop0.example:{}/submit", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(
        err_meta,
        NetGuardError::PostSendBlocked(ref inner)
            if matches!(inner.as_ref(), NetGuardError::BlockedAddress(_))
    ));
    assert!(err_meta.is_post_send_unknown());

    // Contrast: GET -> 302 -> blocked hop1 returns bare BlockedAddress (not PostSendBlocked).
    let err_get = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://hop0.example:{}/read", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(err_get, NetGuardError::BlockedAddress(_)));
    assert!(!err_get.is_post_send_unknown());

    // Direct policy contrast: HTTPS -> HTTP redirect validation returns HttpsDowngradeBlocked.
    let from = net_guard::normalize_url("https://hop0.example/submit", false).unwrap();
    let to = net_guard::normalize_url("http://hop0.example/done", false).unwrap();
    assert!(matches!(
        policy.validate_redirect_hop(&from, &to),
        Err(NetGuardError::HttpsDowngradeBlocked)
    ));
}

#[test]
fn post_oversized_body_returns_post_send_blocked_body_too_large_while_get_returns_body_too_large() {
    let server = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n01234567890123456789".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n01234567890123456789".to_vec(),
    ]);
    let addr = server.addr;
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let client = NetGuardClient::new();

    let err_post = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/submit", addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: Some(8),
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            err_post,
            NetGuardError::PostSendBlocked(ref inner)
                if matches!(inner.as_ref(), NetGuardError::BodyTooLarge { limit: 8 })
        ),
        "POST oversized response must return PostSendBlocked(BodyTooLarge), got {err_post:?}"
    );
    assert!(err_post.is_post_send_unknown());

    let err_get = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/read", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: Some(8),
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(err_get, NetGuardError::BodyTooLarge { limit: 8 }));
    assert!(!err_get.is_post_send_unknown());
}

#[test]
fn hop0_post_connection_refused_preserves_transport() {
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let policy = TargetPolicy::production().with_test_loopback_socket(dead_addr);
    let err = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/attendance.php", dead_addr.port()),
                headers: vec![],
                body: Some(b"ok".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::Transport(_)),
        "Hop-0 POST -> ConnectionRefused must preserve Transport, got {err:?}"
    );
}
