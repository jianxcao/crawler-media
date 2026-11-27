use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use net_guard::{
    DnsResolver, GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, Origin,
    PrivateEndpointException, Scheme, SystemDnsResolver, TargetPolicy, is_cloud_metadata_host,
    is_cloud_metadata_ip, is_global_unicast, normalize_url,
};

use super::support::{FakeResolver, TestServer};

#[test]
fn ip_classification_rejects_all_non_global_and_metadata_ips() {
    let blocked = [
        "0.0.0.0",
        "0.10.0.1",
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.1.1",
        "100.64.0.1",
        "100.127.255.254",
        "169.254.0.1",
        "169.254.169.254",
        "169.254.170.2",
        "100.100.100.200",
        "168.63.129.16",
        "192.0.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fe80::1",
        "fc00::1",
        "fd00:ec2::254",
        "ff02::1",
        "2001:db8::1",
        "2001:2::1",
        "2001::1",
        "2002:7f00:0001::",
        "64:ff9b::127.0.0.1",
        "64:ff9b:1::10.0.0.1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "::ffff:93.184.216.34",
        "::127.0.0.1",
    ];
    for raw in blocked {
        let ip: IpAddr = raw.parse().unwrap();
        assert!(!is_global_unicast(ip), "expected blocked IP: {raw}");
    }

    for meta in [
        "169.254.169.254",
        "169.254.170.2",
        "100.100.100.200",
        "168.63.129.16",
        "fd00:ec2::254",
        "::ffff:169.254.169.254",
        "64:ff9b::169.254.169.254",
        "64:ff9b:1::169.254.169.254",
        "64:ff9b:1:a9fe:a9:fe00::",
    ] {
        let ip: IpAddr = meta.parse().unwrap();
        assert!(is_cloud_metadata_ip(ip), "expected metadata IP: {meta}");
    }

    for meta_host in [
        "metadata",
        "instance-data",
        "metadata.google.internal",
        "sub.metadata.google.internal.",
        "metadata.goog",
        "metadata.azure.internal",
    ] {
        assert!(
            is_cloud_metadata_host(meta_host),
            "expected metadata host: {meta_host}"
        );
    }

    for public in [
        "1.1.1.1",
        "93.184.216.34",
        "2606:2800:220:1:248:1893:25c8:1946",
    ] {
        let ip: IpAddr = public.parse().unwrap();
        assert!(is_global_unicast(ip), "expected global IP: {public}");
    }
}

#[test]
fn url_normalization_rejects_unsafe_inputs_and_canonicalizes_valid_ones() {
    for bad in [
        "",
        "file:///etc/passwd",
        "ftp://example.com/a",
        "javascript:alert(1)",
        "https://user:pass@example.com/",
        "https://user@example.com/",
        "https://*.example.com/",
        "https://example.com/a/../b",
        "https://example.com/%2e%2e/b",
        "https://1.1.1.1/..%2fadmin",
        "https://1.1.1.1/%2e%2e%2fadmin",
        "https://1.1.1.1/..%2Fadmin",
        "https://1.1.1.1/api/..;foo/admin",
        "https://1.1.1.1/api/%2e%2e;foo/admin",
        "https://1.1.1.1/api/..%3bfoo/admin",
        "https://example.com\\evil",
        "https://example.com:0/",
        "https://example.com/\u{200B}path",
        "https://localhost/",
        "https://app.localhost/",
        "https://service.local/",
        "https://db.internal/",
        "http://metadata.google.internal/computeMetadata/v1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://169.254.169.254../latest/meta-data/",
    ] {
        assert!(
            normalize_url(bad, false).is_err(),
            "expected error for {bad}"
        );
    }

    // Double-trailing-dot IP literal must also update host_is_ip when allowed as non-metadata IP.
    let double_dot_ip = normalize_url("http://127.0.0.1../api/v1", true).unwrap();
    assert_eq!(
        double_dot_ip.host_is_ip,
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    );
    // Query parameter containing %2F must remain valid.
    assert!(
        normalize_url(
            "https://example.com/search?next=https%3A%2F%2Fa%2F..%2Fb",
            false
        )
        .is_ok()
    );

    let idna = normalize_url("HTTPS://Münich.Example.:443/search?q=1#frag", false).unwrap();
    assert_eq!(
        idna.origin,
        Origin::new(Scheme::Https, "xn--mnich-kva.example", None)
    );
    assert_eq!(
        idna.url.as_str(),
        "https://xn--mnich-kva.example/search?q=1"
    );
    assert_eq!(idna.port, 443);

    let v6 = normalize_url(
        "https://[2606:2800:0220:0001:0248:1893:25c8:1946]:443/a",
        false,
    )
    .unwrap();
    assert_eq!(
        v6.origin,
        Origin::new(Scheme::Https, "2606:2800:220:1:248:1893:25c8:1946", None)
    );
    assert_eq!(
        v6.origin.to_string(),
        "https://[2606:2800:220:1:248:1893:25c8:1946]"
    );
}

#[test]
fn production_policy_blocks_loopback_and_self_blocks_override_exceptions() {
    let self_addr: SocketAddr = "127.0.0.1:18765".parse().unwrap();
    let other_loopback: SocketAddr = "127.0.0.1:19999".parse().unwrap();

    let prod = TargetPolicy::production();
    let target_other = normalize_url(&format!("http://{other_loopback}/ok"), true).unwrap();
    assert!(matches!(
        prod.validate_socket_addr(&target_other, other_loopback),
        Err(NetGuardError::BlockedAddress(_))
    ));

    let test_pol = TargetPolicy::production()
        .with_blocked_self("0.0.0.0:18765".parse().unwrap())
        .with_test_loopback_socket(other_loopback)
        .with_private_exception(PrivateEndpointException {
            host: "127.0.0.1".into(),
            port: 18765,
            allowed_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            path_prefixes: vec![],
        });

    assert!(
        test_pol
            .validate_socket_addr(&target_other, other_loopback)
            .is_ok()
    );

    let self_target = normalize_url(&format!("http://{self_addr}/api/v1/sites"), true).unwrap();
    assert!(matches!(
        test_pol.validate_socket_addr(&self_target, self_addr),
        Err(NetGuardError::BlockedAddress(_))
    ));

    let v6_self_target = normalize_url("http://[::1]:18765/api/v1/sites", true).unwrap();
    assert!(matches!(
        test_pol.validate_socket_addr(&v6_self_target, "[::1]:18765".parse().unwrap()),
        Err(NetGuardError::BlockedAddress(_))
    ));

    let meta_pol = TargetPolicy::production().with_private_exception(PrivateEndpointException {
        host: "meta-alias.example".into(),
        port: 80,
        allowed_ips: vec![IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))],
        path_prefixes: vec![],
    });
    assert!(matches!(
        normalize_url("http://169.254.169.254/latest", true),
        Err(NetGuardError::BlockedAddress(_))
    ));
    let meta_target = normalize_url("http://meta-alias.example/latest", true).unwrap();
    assert!(matches!(
        meta_pol.validate_socket_addr(&meta_target, "169.254.169.254:80".parse().unwrap()),
        Err(NetGuardError::BlockedAddress(_))
    ));
}

#[test]
fn allowed_origins_path_prefix_blocks_encoded_slash_and_matrix_traversal_before_connect() {
    let mut server = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let addr = server.addr;
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_allowed_origin(
            Origin::new(Scheme::Http, "127.0.0.1", Some(addr.port())),
            vec!["/api/".into()],
        );
    let client = NetGuardClient::new();

    for traversal_path in [
        "/api/..%2fadmin",
        "/api/..%2Fadmin",
        "/api/%2e%2e%2fadmin",
        "/api/..;foo/admin",
        "/api/%2f../admin",
    ] {
        let err = client
            .execute(
                GuardedRequest {
                    method: GuardedMethod::Get,
                    url: format!("http://127.0.0.1:{}{}", addr.port(), traversal_path),
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
            matches!(err, NetGuardError::InvalidUrl(_)),
            "expected InvalidUrl for `{traversal_path}`, got {err:?}"
        );
    }

    let ok_resp = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/api/status?q=%2F", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap();
    assert_eq!(ok_resp.status, 200);
    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].starts_with("GET /api/status?q=%2F "));
}

#[test]
fn self_blocked_canonicalizes_ipv4_mapped_ipv6_and_unspecified_addrs() {
    let target = normalize_url("http://alias.example:18765/api/v1/sites", true).unwrap();
    let policy = TargetPolicy::production()
        .with_blocked_self("127.0.0.1:18765".parse().unwrap())
        .with_test_loopback_socket("[::ffff:127.0.0.1]:18765".parse().unwrap())
        .with_test_loopback_socket("[::127.0.0.1]:18765".parse().unwrap())
        .with_test_loopback_socket("[::1]:18765".parse().unwrap())
        .with_test_loopback_socket("0.0.0.0:18765".parse().unwrap())
        .with_test_loopback_socket("[::]:18765".parse().unwrap())
        .with_test_loopback_socket("[::ffff:0.0.0.0]:18765".parse().unwrap())
        .with_test_loopback_socket("127.0.0.1:19999".parse().unwrap());

    for blocked in [
        "[::ffff:127.0.0.1]:18765",
        "[::127.0.0.1]:18765",
        "[::1]:18765",
        "0.0.0.0:18765",
        "[::]:18765",
        "[::ffff:0.0.0.0]:18765",
    ] {
        let sock: SocketAddr = blocked.parse().unwrap();
        assert!(
            matches!(
                policy.validate_socket_addr(&target, sock),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "expected {blocked} to be blocked against self_listen 127.0.0.1:18765"
        );
    }

    for self_alias in ["[::]:18765", "[::ffff:0.0.0.0]:18765", "[::1]:18765"] {
        let p = TargetPolicy::production()
            .with_blocked_self(self_alias.parse().unwrap())
            .with_test_loopback_socket("127.0.0.1:18765".parse().unwrap())
            .with_test_loopback_socket("[::ffff:127.0.0.1]:18765".parse().unwrap());
        for probe in ["127.0.0.1:18765", "[::ffff:127.0.0.1]:18765"] {
            assert!(
                matches!(
                    p.validate_socket_addr(&target, probe.parse().unwrap()),
                    Err(NetGuardError::BlockedAddress(_))
                ),
                "expected {probe} blocked against {self_alias}"
            );
        }
    }

    let ok_other: SocketAddr = "127.0.0.1:19999".parse().unwrap();
    let ok_target = normalize_url("http://127.0.0.1:19999/ok", true).unwrap();
    assert!(policy.validate_socket_addr(&ok_target, ok_other).is_ok());
}

#[test]
fn cross_origin_redirect_never_captures_set_cookie_when_creds_is_none() {
    let mut server_b = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nSet-Cookie: cross_sid=evil; Path=/\r\nContent-Length: 4\r\n\r\nland"
            .to_vec(),
    ]);
    let addr_b = server_b.addr;

    let mut server_a = TestServer::spawn(vec![
        format!(
            "HTTP/1.1 302 Found\r\nSet-Cookie: initial_sid=good; Path=/\r\nLocation: http://127.0.0.1:{}/land\r\nContent-Length: 0\r\n\r\n",
            addr_b.port()
        )
        .into_bytes(),
    ]);
    let addr_a = server_a.addr;

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
            None,
        )
        .unwrap();

    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"land");
    assert_eq!(
        resp.captured_set_cookies,
        vec!["initial_sid=good; Path=/".to_string()],
        "cross-origin redirect hop Set-Cookie must never be captured even when creds is None"
    );
    assert!(
        !resp
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("set-cookie")),
        "Set-Cookie must remain stripped from sanitized headers"
    );
    assert_eq!(server_a.take_requests().len(), 1);
    assert_eq!(server_b.take_requests().len(), 1);
}

#[test]
fn private_exception_scopes_private_hostname_allowance_to_matching_host_and_port() {
    let policy = TargetPolicy::production().with_private_exception(PrivateEndpointException {
        host: "nas.local".into(),
        port: 8080,
        allowed_ips: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))],
        path_prefixes: vec!["/api".into()],
    });

    assert!(policy.allows_private_hostname_for("nas.local", 8080));
    assert!(policy.allows_private_hostname_for("NAS.LOCAL", 8080));
    assert!(!policy.allows_private_hostname_for("nas.local", 9090));
    assert!(!policy.allows_private_hostname_for("other.local", 8080));
    assert!(
        policy
            .normalize_url("http://nas.local:8080/api/ping")
            .is_ok()
    );

    for unrelated in [
        "http://nas.local:9090/api/ping",
        "http://other.local:8080/api/ping",
        "http://db.internal:8080/admin",
        "http://app.localhost:8080/admin",
        "http://localhost:8080/admin",
        "http://intranet:8080/admin",
    ] {
        assert!(
            matches!(
                policy.normalize_url(unrelated),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "unrelated private hostname `{unrelated}` must still be blocked"
        );
    }

    let resolver = Arc::new(FakeResolver::new(HashMap::from([(
        "db.internal".into(),
        vec![vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))]],
    )])));
    let err = NetGuardClient::with_resolver(resolver.clone())
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: "http://db.internal:8080/admin".into(),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, NetGuardError::BlockedAddress(_)));
    assert_eq!(
        resolver.call_count(),
        0,
        "blocked unrelated .internal host must be rejected before DNS resolution"
    );

    let ips = SystemDnsResolver
        .resolve("127.0.0.1", 80, Duration::from_millis(500))
        .unwrap();
    assert_eq!(ips, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
}

#[test]
fn benchmark_fake_ip_domains_opt_in_allows_domain_resolution_but_blocks_ip_literals_and_private_ranges()
 {
    let prod = TargetPolicy::production();
    let domain_target = normalize_url("https://hdarea.club/torrents.php", true).unwrap();
    let fake_ip_1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 32)), 443);
    let fake_ip_2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 19, 255, 254)), 443);
    assert!(
        matches!(
            prod.validate_socket_addr(&domain_target, fake_ip_1),
            Err(NetGuardError::BlockedAddress(_))
        ),
        "default production policy must block 198.18.0.0/15"
    );

    let self_fake_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 32)), 18765);
    let fake_ip_policy = TargetPolicy::production()
        .with_benchmark_fake_ip_domains(true)
        .with_blocked_self(self_fake_ip);

    assert!(
        fake_ip_policy
            .validate_socket_addr(&domain_target, fake_ip_1)
            .is_ok(),
        "domain resolved to 198.18.0.32:443 must be permitted when opted in"
    );
    assert!(
        fake_ip_policy
            .validate_socket_addr(&domain_target, fake_ip_2)
            .is_ok(),
        "domain resolved to 198.19.255.254:443 must be permitted when opted in"
    );

    // Literal IP URLs in 198.18.0.0/15 must still be rejected.
    let ip_literal_target = normalize_url("https://198.18.0.32/torrents.php", true).unwrap();
    assert!(matches!(
        fake_ip_policy.validate_socket_addr(&ip_literal_target, fake_ip_1),
        Err(NetGuardError::BlockedAddress(_))
    ));

    // IPv4-mapped IPv6 addresses must NOT bypass via benchmark fake-IP allowance.
    let v6_mapped = SocketAddr::new("::ffff:198.18.0.32".parse().unwrap(), 443);
    assert!(matches!(
        fake_ip_policy.validate_socket_addr(&domain_target, v6_mapped),
        Err(NetGuardError::BlockedAddress(_))
    ));

    // Private hostnames and non-fake-ip private/metadata ranges remain blocked.
    for private_url in ["http://localhost/", "http://nas.local/"] {
        assert!(matches!(
            fake_ip_policy.normalize_url(private_url),
            Err(NetGuardError::BlockedAddress(_))
        ));
    }
    for blocked_ip in [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 2)), 443),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), 443),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 80),
    ] {
        assert!(matches!(
            fake_ip_policy.validate_socket_addr(&domain_target, blocked_ip),
            Err(NetGuardError::BlockedAddress(_))
        ));
    }

    // blocked_self overrides even fake-IP allowance.
    let self_domain = normalize_url("http://hdarea.club:18765/", true).unwrap();
    assert!(matches!(
        fake_ip_policy.validate_socket_addr(&self_domain, self_fake_ip),
        Err(NetGuardError::BlockedAddress(_))
    ));
}
