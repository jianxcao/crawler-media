use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use net_guard::{
    DnsResolver, GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, NetGuardLimits,
    PinnedResolver, PinnedTcpConnector, PrivateEndpointException, SystemDnsResolver, TargetPolicy,
};

use super::support::TestServer;

#[derive(Debug)]
struct SlowRecordingResolver {
    delays: HashMap<String, Duration>,
    recorded_timeouts: Mutex<Vec<(String, Duration)>>,
}

impl DnsResolver for SlowRecordingResolver {
    fn resolve(
        &self,
        host: &str,
        _port: u16,
        timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError> {
        self.recorded_timeouts
            .lock()
            .unwrap()
            .push((host.to_string(), timeout));
        if timeout.is_zero() {
            return Err(NetGuardError::Timeout(
                "DNS resolution deadline expired".into(),
            ));
        }
        let delay = self
            .delays
            .get(host)
            .copied()
            .unwrap_or(Duration::from_millis(0));
        thread::sleep(delay);
        Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    }
}

#[test]
fn system_dns_resolver_zero_timeout_returns_timeout_immediately() {
    let resolver = SystemDnsResolver;
    let started = Instant::now();
    let err = resolver
        .resolve("127.0.0.1", 80, Duration::ZERO)
        .unwrap_err();
    assert!(
        matches!(err, NetGuardError::Timeout(_)),
        "expected NetGuardError::Timeout for Duration::ZERO, got {err:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(50));
}

#[test]
fn slow_dns_deducts_from_global_timeout_before_connect() {
    let mut server = TestServer::spawn(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let addr = server.addr;
    let resolver = Arc::new(SlowRecordingResolver {
        delays: HashMap::from([("slow.example".into(), Duration::from_millis(75))]),
        recorded_timeouts: Mutex::new(Vec::new()),
    });
    let client = NetGuardClient::with_resolver(resolver.clone());
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_limits(NetGuardLimits {
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            global_timeout: Duration::from_millis(50),
            ..NetGuardLimits::default()
        });
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://slow.example:{}/", addr.port()),
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
        matches!(err, NetGuardError::Timeout(_)),
        "expected Timeout after slow DNS exhausted global_timeout, got {err:?}"
    );
    assert!(
        server.take_requests().is_empty(),
        "must not dial server when post-DNS deadline already expired"
    );
    let recorded = resolver.recorded_timeouts.lock().unwrap();
    assert!(recorded[0].1 <= Duration::from_millis(50));
}

#[test]
fn slow_dns_deducts_from_deadline_across_redirect_hops() {
    for (method, expect_post_send) in [(GuardedMethod::Get, false), (GuardedMethod::Post, true)] {
        let mut server_b = TestServer::spawn(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
        ]);
        let addr_b = server_b.addr;
        let server_a = TestServer::spawn(vec![
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://hop2.example:{}/done\r\nContent-Length: 0\r\n\r\n",
                addr_b.port()
            )
            .into_bytes(),
        ]);
        let addr_a = server_a.addr;
        let resolver = Arc::new(SlowRecordingResolver {
            delays: HashMap::from([
                ("hop1.example".into(), Duration::from_millis(50)),
                ("hop2.example".into(), Duration::from_millis(250)),
            ]),
            recorded_timeouts: Mutex::new(Vec::new()),
        });
        let client = NetGuardClient::with_resolver(resolver.clone());
        let policy = TargetPolicy::production()
            .with_test_loopback_socket(addr_a)
            .with_test_loopback_socket(addr_b)
            .with_limits(NetGuardLimits {
                resolve_timeout: Duration::from_secs(5),
                connect_timeout: Duration::from_secs(5),
                global_timeout: Duration::from_millis(220),
                ..NetGuardLimits::default()
            });
        let err = client
            .execute(
                GuardedRequest {
                    method,
                    url: format!("http://hop1.example:{}/start", addr_a.port()),
                    headers: vec![],
                    body: if expect_post_send {
                        Some(b"payload".to_vec())
                    } else {
                        None
                    },
                    proxy: None,
                    max_response_bytes: None,
                },
                &policy,
                None,
            )
            .unwrap_err();
        if expect_post_send {
            assert!(
                matches!(err, NetGuardError::PostSendUnknown(_)),
                "expected PostSendUnknown after mutating hop1 + slow hop2 DNS, got {err:?}"
            );
        } else {
            assert!(
                matches!(err, NetGuardError::Timeout(_)),
                "expected Timeout after hop1 + slow hop2 DNS, got {err:?}"
            );
        }
        assert!(
            server_b.take_requests().is_empty(),
            "hop2 server must not receive request after post-DNS deadline expired"
        );
        let timeouts = resolver.recorded_timeouts.lock().unwrap();
        assert_eq!(timeouts.len(), 2);
        assert!(
            timeouts[1].1 < timeouts[0].1,
            "hop2 DNS timeout {:?} must be clamped below hop1 {:?}",
            timeouts[1].1,
            timeouts[0].1
        );
    }
}

#[derive(Debug)]
struct StaticMultiIpResolver(Vec<IpAddr>);

impl DnsResolver for StaticMultiIpResolver {
    fn resolve(&self, _: &str, _: u16, _: Duration) -> Result<Vec<IpAddr>, NetGuardError> {
        Ok(self.0.clone())
    }
}

#[test]
fn multi_record_blackhole_connect_stays_within_global_timeout() {
    let blackhole_ips: Vec<IpAddr> = (1..=5)
        .map(|last| IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)))
        .collect();
    let resolver = Arc::new(StaticMultiIpResolver(blackhole_ips.clone()));
    let client = NetGuardClient::with_resolver(resolver);
    let policy = TargetPolicy::production()
        .with_private_exception(PrivateEndpointException {
            host: "blackhole.example".into(),
            port: 81,
            allowed_ips: blackhole_ips,
            path_prefixes: vec!["/".into()],
        })
        .with_limits(NetGuardLimits {
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_millis(250),
            global_timeout: Duration::from_millis(180),
            ..NetGuardLimits::default()
        });

    let started = Instant::now();
    let err = client
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: "http://blackhole.example:81/".into(),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, NetGuardError::Timeout(_)),
        "expected Timeout when multi-record connect exhausts global_timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_millis(450),
        "5 blackholed IPs must share global_timeout (180ms) rather than 5 * timeout, took {elapsed:?}"
    );
}

#[test]
fn expired_zero_budget_connect_returns_timeout_without_one_second_grace() {
    let blackhole_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    let addr = SocketAddr::new(blackhole_ip, 81);
    let policy = TargetPolicy::production()
        .with_private_exception(PrivateEndpointException {
            host: "blackhole.example".into(),
            port: 81,
            allowed_ips: vec![blackhole_ip],
            path_prefixes: vec!["/".into()],
        })
        .with_limits(NetGuardLimits {
            connect_timeout: Duration::from_secs(5),
            ..NetGuardLimits::default()
        });
    let target = policy
        .normalize_url("http://blackhole.example:81/")
        .unwrap();
    let uri: ureq::http::Uri = "http://blackhole.example:81/".parse().unwrap();
    let config = ureq::config::Config::builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_connect(Some(Duration::ZERO))
        .timeout_global(Some(Duration::from_secs(5)))
        .build();
    let connector = PinnedTcpConnector {
        addrs: vec![addr],
        target: target.clone(),
        policy: policy.clone(),
    };
    let resolver = PinnedResolver {
        expected_uri: uri.clone(),
        addrs: vec![addr],
    };
    let agent = ureq::Agent::with_parts(config, connector, resolver);
    let req = ureq::http::Request::builder()
        .method("GET")
        .uri(uri.clone())
        .body(())
        .unwrap();

    let started = Instant::now();
    let err = agent.run(req).unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, ureq::Error::Timeout(_)),
        "expected ureq::Error::Timeout on zero-budget connect, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_millis(150),
        "zero-budget connect must not receive a 1s grace period, took {elapsed:?}"
    );

    let zero_policy = policy.with_limits(NetGuardLimits {
        connect_timeout: Duration::ZERO,
        ..NetGuardLimits::default()
    });
    let agent_unbounded = ureq::Agent::with_parts(
        ureq::config::Config::builder()
            .proxy(None)
            .timeout_connect(None)
            .build(),
        PinnedTcpConnector {
            addrs: vec![addr],
            target,
            policy: zero_policy,
        },
        PinnedResolver {
            expected_uri: uri.clone(),
            addrs: vec![addr],
        },
    );
    let started_zero = Instant::now();
    let err_zero = agent_unbounded
        .run(
            ureq::http::Request::builder()
                .method("GET")
                .uri(uri)
                .body(())
                .unwrap(),
        )
        .unwrap_err();
    assert!(matches!(err_zero, ureq::Error::Timeout(_)));
    assert!(started_zero.elapsed() < Duration::from_millis(150));
}

#[test]
fn slow_drip_body_aborts_at_wall_clock_global_deadline() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let drip_srv = thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut req_buf = [0u8; 512];
            let _ = stream.read(&mut req_buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 32\r\n\r\n");
            let _ = stream.flush();
            for _ in 0..14 {
                thread::sleep(Duration::from_millis(60));
                if stream.write_all(b"x").is_err() || stream.flush().is_err() {
                    break;
                }
            }
        }
    });

    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_limits(NetGuardLimits {
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            global_timeout: Duration::from_millis(220),
            ..NetGuardLimits::default()
        });
    let started = Instant::now();
    let err = NetGuardClient::new()
        .execute(
            GuardedRequest {
                method: GuardedMethod::Get,
                url: format!("http://127.0.0.1:{}/drip", addr.port()),
                headers: vec![],
                body: None,
                proxy: None,
                max_response_bytes: None,
            },
            &policy,
            None,
        )
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, NetGuardError::Timeout(_)),
        "expected NetGuardError::Timeout on slow-drip body, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "slow-drip body must abort near global_timeout (220ms), took {elapsed:?}"
    );

    let target = policy
        .normalize_url(&format!("http://127.0.0.1:{}/drip", addr.port()))
        .unwrap();
    let uri: ureq::http::Uri = target.url.as_str().parse().unwrap();
    let direct_agent = ureq::Agent::with_parts(
        ureq::config::Config::builder()
            .proxy(None)
            .timeout_recv_body(Some(Duration::from_millis(150)))
            .timeout_global(Some(Duration::from_millis(220)))
            .build(),
        PinnedTcpConnector {
            addrs: vec![addr],
            target,
            policy: policy.clone(),
        },
        PinnedResolver {
            expected_uri: uri.clone(),
            addrs: vec![addr],
        },
    );
    let started_direct = Instant::now();
    let mut resp = direct_agent
        .run(
            ureq::http::Request::builder()
                .method("GET")
                .uri(uri)
                .body(())
                .unwrap(),
        )
        .unwrap();
    let mut out = Vec::new();
    let io_err = resp
        .body_mut()
        .as_reader()
        .read_to_end(&mut out)
        .unwrap_err();
    assert!(
        started_direct.elapsed() < Duration::from_millis(500),
        "direct PinnedTcpConnector slow-drip took {:?}: {io_err:?}",
        started_direct.elapsed()
    );
    drip_srv.join().unwrap();
}

#[test]
fn slow_drip_tls_handshake_aborts_at_wall_clock_connect_and_global_deadlines() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    for (connect_timeout, global_timeout, expect_global) in [
        (Duration::from_millis(180), Duration::from_secs(2), false),
        (Duration::from_secs(5), Duration::from_millis(220), true),
    ] {
        let tls_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tls_addr = tls_listener.local_addr().unwrap();
        let tls_srv = thread::spawn(move || {
            let Ok((mut stream, _)) = tls_listener.accept() else {
                return;
            };
            let mut ch_buf = [0u8; 512];
            let _ = stream.read(&mut ch_buf);
            let tls_header = [
                0x16_u8, 0x03, 0x03, 0x00, 0x40, 0x02, 0x00, 0x00, 0x3c, 0x03, 0x03, 0, 0, 0,
            ];
            for b in tls_header {
                thread::sleep(Duration::from_millis(60));
                if stream.write_all(&[b]).is_err() || stream.flush().is_err() {
                    break;
                }
            }
        });
        let tls_resolver = Arc::new(StaticMultiIpResolver(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]));
        let tls_policy = TargetPolicy::production()
            .with_test_loopback_socket(tls_addr)
            .with_limits(NetGuardLimits {
                connect_timeout,
                global_timeout,
                ..NetGuardLimits::default()
            });
        let tls_started = Instant::now();
        let tls_err = NetGuardClient::with_resolver(tls_resolver)
            .execute(
                GuardedRequest {
                    method: GuardedMethod::Get,
                    url: format!("https://drip-tls.example:{}/", tls_addr.port()),
                    headers: vec![],
                    body: None,
                    proxy: None,
                    max_response_bytes: None,
                },
                &tls_policy,
                None,
            )
            .unwrap_err();
        if expect_global {
            assert!(
                matches!(tls_err, NetGuardError::Timeout(_)),
                "expected NetGuardError::Timeout on slow-drip global_timeout, got {tls_err:?}"
            );
        } else {
            assert!(
                matches!(tls_err, NetGuardError::Transport(_)),
                "expected Transport(timeout: Connect) on slow-drip connect_timeout, got {tls_err:?}"
            );
        }
        assert!(
            tls_started.elapsed() < Duration::from_millis(500),
            "slow-drip must abort near wall-clock deadline (expect_global={expect_global}), took {:?}",
            tls_started.elapsed()
        );
        tls_srv.join().unwrap();
    }
}
