use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use flate2::Compression;
use flate2::write::GzEncoder;
use net_guard::{
    CredentialSlot, GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, NetGuardLimits,
    Origin, Scheme, TargetPolicy, normalize_url,
};

use super::support::{FakeResolver, TestServer};

#[test]
fn dns_rebinding_mixed_ips_rejected_and_pinned_single_resolve_preserves_host() {
    let mut server = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: sid=secret; HttpOnly\r\n\r\nok"
            .to_vec(),
    ]);
    let addr = server.addr;

    // Mixed public + loopback DNS response must be rejected immediately.
    let mixed_resolver = Arc::new(FakeResolver::new(HashMap::from([(
        "mixed.example".into(),
        vec![vec![
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ]],
    )])));
    let mixed_client = NetGuardClient::with_resolver(mixed_resolver);
    let err = mixed_client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://mixed.example:{}/", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &TargetPolicy::production(),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::BlockedAddress(_)),
        "got {err:?}"
    );

    // Rebinding attempt: hop resolves once to allowed loopback, second lookup would return blocked IP.
    let rebind_resolver = Arc::new(FakeResolver::new(HashMap::from([(
        "rebind.example".into(),
        vec![
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            vec![IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))],
        ],
    )])));
    let client = NetGuardClient::with_resolver(rebind_resolver.clone());
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let resp = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://rebind.example:{}/hello", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap();

    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"ok");
    assert_eq!(rebind_resolver.call_count(), 1);
    assert_eq!(
        resp.captured_set_cookies,
        vec!["sid=secret; HttpOnly".to_string()]
    );
    assert!(
        !resp
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("set-cookie")),
        "Set-Cookie must be stripped from sanitized headers"
    );

    let reqs = server.take_requests();
    let raw_req = &reqs[0];
    let expected_host = format!("host: rebind.example:{}", addr.port());
    assert!(
        raw_req.to_ascii_lowercase().contains(&expected_host),
        "expected Host header `{expected_host}` in:\n{raw_req}"
    );
}

#[test]
fn credential_slot_forbids_caller_auth_headers_and_never_leaks_cross_origin() {
    let origin_b_resp = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecond".to_vec();
    let mut server_b = TestServer::spawn(vec![origin_b_resp]);
    let addr_b = server_b.addr;

    let origin_a_resp = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/next\r\nContent-Length: 0\r\n\r\n",
        addr_b.port()
    )
    .into_bytes();
    let mut server_a = TestServer::spawn(vec![origin_a_resp]);
    let addr_a = server_a.addr;

    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr_a)
        .with_test_loopback_socket(addr_b);
    let creds = CredentialSlot::new(
        "site-1",
        Origin::new(Scheme::Http, "127.0.0.1", Some(addr_a.port())),
        "epoch-1",
        vec![
            ("Cookie".into(), "uid=secret-cookie".into()),
            ("x-api-key".into(), "secret-api-key".into()),
        ],
    );
    assert!(!format!("{creds:?}").contains("secret-cookie"));
    assert!(!format!("{creds:?}").contains("secret-api-key"));

    let client = NetGuardClient::new();
    // Caller-supplied Cookie / Host / Authorization must be forbidden.
    for forbidden in [
        "Cookie",
        "Host",
        "Authorization",
        "Proxy-Authorization",
        "x-api-key",
    ] {
        let err = client
            .execute(
                GuardedRequest {
                    method: GuardedMethod::Get,
                    url: format!("http://127.0.0.1:{}/", addr_a.port()),
                    headers: vec![(forbidden.into(), "evil".into())],
                    body: None,
                    proxy: None,
                    max_response_bytes: None,
                },
                &policy,
                Some(&creds),
            )
            .unwrap_err();
        assert!(
            matches!(err, NetGuardError::HeaderForbidden(_)),
            "got {err:?}"
        );
    }

    let resp = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/start", addr_a.port()),
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: Some(b"{}".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            Some(&creds),
        )
        .unwrap();

    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"second");

    let reqs_a = server_a.take_requests();
    let req_a = &reqs_a[0];
    assert!(
        req_a.contains("uid=secret-cookie"),
        "origin A must get cookie: {req_a}"
    );
    assert!(
        req_a.contains("secret-api-key"),
        "origin A must get api key: {req_a}"
    );

    let reqs_b = server_b.take_requests();
    let req_b = &reqs_b[0];
    assert!(
        req_b.starts_with("GET /next"),
        "302 must rewrite POST->GET: {req_b}"
    );
    assert!(
        !req_b.to_ascii_lowercase().contains("secret-cookie")
            && !req_b.to_ascii_lowercase().contains("secret-api-key"),
        "credentials leaked across origins: {req_b}"
    );
}

#[test]
fn redirect_blocks_https_downgrade_private_targets_and_more_than_five_hops() {
    // 6 redirects (> 5 max) -> TooManyRedirects.
    let responses: Vec<Vec<u8>> = (0..6)
        .map(|i| {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: /hop{}\r\nContent-Length: 0\r\n\r\n",
                i + 1
            )
            .into_bytes()
        })
        .collect();
    let server = TestServer::spawn(responses);
    let addr = server.addr;
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let client = NetGuardClient::new();
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/hop0", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::TooManyRedirects(5)),
        "got {err:?}"
    );

    // Redirect to blocked metadata IP must fail closed.
    let server2 = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    ]);
    let addr2 = server2.addr;
    let err2 = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/", addr2.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &TargetPolicy::production().with_test_loopback_socket(addr2),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err2, NetGuardError::BlockedAddress(_)),
        "got {err2:?}"
    );

    // HTTPS -> HTTP downgrade must be blocked.
    let from = normalize_url("https://example.com/a", false).unwrap();
    let to = normalize_url("http://example.com/b", false).unwrap();
    let downgrade = TargetPolicy::production().validate_redirect_hop(&from, &to);
    assert!(
        matches!(downgrade, Err(NetGuardError::HttpsDowngradeBlocked)),
        "got {downgrade:?}"
    );

    // 307 redirect preserves POST method and body on same origin.
    let mut server3 = TestServer::spawn(vec![
        b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
    ]);
    let addr3 = server3.addr;
    let resp3 = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/start", addr3.port()),
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: Some(b"{\"k\":1}".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &TargetPolicy::production().with_test_loopback_socket(addr3),
            None,
        )
        .unwrap();
    assert_eq!(resp3.body, b"done");
    let reqs3 = server3.take_requests();
    assert!(
        reqs3[1].starts_with("POST /final"),
        "307 must preserve POST: {}",
        reqs3[1]
    );
    assert!(
        reqs3[1].contains("{\"k\":1}"),
        "307 must preserve body: {}",
        reqs3[1]
    );
}

#[test]
fn https_never_disables_tls_certificate_verification() {
    let server = TestServer::spawn(vec![b"NOT_TLS_DATA\r\n\r\n".to_vec()]);
    let addr = server.addr;
    let resolver = Arc::new(FakeResolver::new(HashMap::from([(
        "tls.example".into(),
        vec![vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]],
    )])));
    let client = NetGuardClient::with_resolver(resolver);
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("https://tls.example:{}/", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::Transport(_)),
        "expected TLS failure when peer is not valid TLS, got {err:?}"
    );
}

#[test]
fn external_proxy_fails_closed_and_payload_limits_enforce_wire_and_decompressed_caps() {
    let client = NetGuardClient::new();
    let proxy_err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: "https://example.com/".into(),
                headers: vec![],
                body: None,
                proxy: Some("http://127.0.0.1:8888".into()),
                max_response_bytes: None,
            },
            &TargetPolicy::production(),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(proxy_err, NetGuardError::UnsupportedRoute(_)),
        "got {proxy_err:?}"
    );

    // Request > 1 MiB rejected before dial.
    let req_err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: "https://example.com/".into(),
                headers: vec![],
                body: Some(vec![b'a'; 1_048_577]),
                proxy: None,
                max_response_bytes: None,
            },
            &TargetPolicy::production(),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(req_err, NetGuardError::RequestTooLarge { .. }),
        "got {req_err:?}"
    );

    // Gzip bomb: small wire size, decompressed > 1024 limit.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&vec![b'A'; 4096]).unwrap();
    let compressed = encoder.finish().unwrap();
    assert!(compressed.len() < 512);
    let mut http_resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
        compressed.len()
    )
    .into_bytes();
    http_resp.extend_from_slice(&compressed);

    let server = TestServer::spawn(vec![http_resp]);
    let addr = server.addr;
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_limits(NetGuardLimits {
            max_response_bytes: 1024,
            ..NetGuardLimits::default()
        });
    let bomb_err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/bomb", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(bomb_err, NetGuardError::BodyTooLarge { limit: 1024 }),
        "got {bomb_err:?}"
    );
    assert_eq!(NetGuardLimits::torrent().max_response_bytes, 10_485_760);
    let _ = Ipv6Addr::LOCALHOST;
}

#[test]
fn redirect_enforces_path_prefix_segment_boundary() {
    let mut server = TestServer::spawn(vec![
        b"HTTP/1.1 302 Found\r\nLocation: /api-secret/dump\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret".to_vec(),
    ]);
    let addr = server.addr;
    let origin = Origin::new(Scheme::Http, "127.0.0.1", Some(addr.port()));
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_allowed_origin(origin, vec!["/api".into()]);
    let client = NetGuardClient::new();
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/api/go", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::BlockedAddress(_)),
        "expected BlockedAddress on /api-secret/dump redirect, got {err:?}"
    );
    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
}

#[test]
fn connector_survives_peer_rst_after_sending_http_response() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            // Send full response immediately while leaving remaining POST body unread -> triggers RST on drop on XNU.
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            let _ = stream.flush();
            drop(stream);
        }
    });

    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let client = NetGuardClient::new();
    for _ in 0..5 {
        let resp = client
            .execute(
                GuardedRequest {
                    method: GuardedMethod::Post,
                    url: format!("http://127.0.0.1:{}/fast", addr.port()),
                    headers: vec![],
                    body: Some(vec![b'x'; 8192]),
                    proxy: None,
                    max_response_bytes: None,
                },
                &policy,
                None,
            )
            .expect("buffered HTTP 200 should survive peer RST/EINVAL on setsockopt");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");
    }
    handle.join().unwrap();
}

#[test]
fn redirect_enforces_per_origin_methods_across_307_and_302() {
    let mut srv_b = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
    ]);
    let addr_b = srv_b.addr;
    let mut srv_a = TestServer::spawn(vec![
        format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{addr_b}/admin\r\nContent-Length: 0\r\n\r\n").into_bytes(),
        format!("HTTP/1.1 302 Found\r\nLocation: http://{addr_b}/admin\r\nContent-Length: 0\r\n\r\n").into_bytes(),
        b"HTTP/1.1 302 Found\r\nLocation: /post-only\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    let addr_a = srv_a.addr;
    let origin_a = Origin::new(Scheme::Http, "127.0.0.1", Some(addr_a.port()));
    let origin_b = Origin::new(Scheme::Http, "127.0.0.1", Some(addr_b.port()));
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr_a)
        .with_test_loopback_socket(addr_b)
        .with_allowed_origin_methods(origin_a.clone(), vec!["/go".into()], vec!["POST".into()])
        .with_allowed_origin_methods(origin_a, vec!["/post-only".into()], vec!["POST".into()])
        .with_allowed_origin_methods(origin_b, vec!["/admin".into()], vec!["GET".into()]);

    let client = NetGuardClient::new();
    let err_307 = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/go", addr_a.port()),
                headers: vec![],
                body: Some(b"x".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(
        err_307,
        NetGuardError::PostSendBlocked(ref inner)
            if matches!(inner.as_ref(), NetGuardError::MethodNotAllowed(_))
    ));
    assert!(err_307.is_post_send_unknown());

    let ok_302 = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/go", addr_a.port()),
                headers: vec![],
                body: Some(b"x".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap();
    assert_eq!(ok_302.body, b"done");

    let err_302_post_only = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/go", addr_a.port()),
                headers: vec![],
                body: Some(b"x".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(
        err_302_post_only,
        NetGuardError::PostSendBlocked(ref inner)
            if matches!(inner.as_ref(), NetGuardError::MethodNotAllowed(_))
    ));
    assert!(err_302_post_only.is_post_send_unknown());

    assert_eq!(srv_a.take_requests().len(), 3);
    let reqs_b = srv_b.take_requests();
    assert_eq!(reqs_b.len(), 1);
    assert!(reqs_b[0].starts_with("GET /admin"));
}

#[test]
fn post_send_eof_on_mutating_method_returns_post_send_unknown_while_connect_refused_returns_transport()
 {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for i in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            if i == 1 {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nhi");
                let _ = stream.flush();
            }
            drop(stream);
        }
    });

    let client = NetGuardClient::new();
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    for _ in 0..2 {
        let err = client
            .execute(
                GuardedRequest {
                    method: GuardedMethod::Post,
                    url: format!("http://127.0.0.1:{}/attendance.php", addr.port()),
                    headers: vec![],
                    body: Some(b"".to_vec()),
                    proxy: None,
                    max_response_bytes: None,
                },
                &policy,
                None,
            )
            .unwrap_err();
        assert!(
            matches!(err, NetGuardError::PostSendUnknown(_)),
            "expected PostSendUnknown on post-write EOF, got {err:?}"
        );
    }
    handle.join().unwrap();

    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let dead_policy = TargetPolicy::production().with_test_loopback_socket(dead_addr);
    let err_refused = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Post,
                url: format!("http://127.0.0.1:{}/attendance.php", dead_addr.port()),
                headers: vec![],
                body: Some(b"".to_vec()),
                proxy: None,
                max_response_bytes: None,
            },
            &dead_policy,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err_refused, NetGuardError::Transport(_)),
        "expected Transport on pre-send connect refused, got {err_refused:?}"
    );
}

#[test]
fn post_redirect_get_mid_stream_eof_returns_post_send_unknown() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    for partial_body in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            // Hop 1: POST receives 302 redirect to /done.
            let (mut s1, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s1.read(&mut buf);
            let _ =
                s1.write_all(b"HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\n\r\n");
            let _ = s1.flush();
            drop(s1);

            // Hop 2: redirected GET encounters mid-stream EOF (header or partial body).
            let (mut s2, _) = listener.accept().unwrap();
            let _ = s2.read(&mut buf);
            if partial_body {
                let _ = s2.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nhi");
                let _ = s2.flush();
            }
            drop(s2);
        });

        let client = NetGuardClient::new();
        let policy = TargetPolicy::production().with_test_loopback_socket(addr);
        let err = client
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
            "expected PostSendUnknown on PRG redirected GET EOF (partial_body={partial_body}), got {err:?}"
        );
        handle.join().unwrap();
    }
}
