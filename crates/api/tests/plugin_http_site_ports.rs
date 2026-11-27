#[path = "plugin_http_site_ports/public_url.rs"]
mod public_url;
#[path = "plugin_commands/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use api::ApiState;
use domain::{MaintenanceVia, Site, SiteId};
use indexer::{IndexerError, IndexerProvider, ProviderCallContext, SiteFetch};
use net_guard::TargetPolicy;
use plugin_api::{
    ErrorCode, ErrorDetail, GuestRole, HttpRequest, HttpTarget, PageRequest, SiteTarget,
};
use plugin_host::{Invocation, WasmIndexerConfig, WasmIndexerProvider};
use store::plugins::*;
use support::*;

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

pub(crate) fn spawn_http_server<F>(
    handler: F,
) -> (SocketAddr, Arc<AtomicUsize>, std::thread::JoinHandle<()>)
where
    F: Fn(&str, &str) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_ref = hits.clone();
    listener.set_nonblocking(false).unwrap();
    let handle = std::thread::spawn(move || {
        // Keep accepting up to 16 requests or until idle timeout
        let _ = listener.set_nonblocking(false);
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 8192];
            let mut n = 0;
            while n < buf.len() {
                match stream.read(&mut buf[n..]) {
                    Ok(0) | Err(_) => break,
                    Ok(read_bytes) => {
                        n += read_bytes;
                        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            if n == 0 {
                continue;
            }
            let raw = String::from_utf8_lossy(&buf[..n]);
            let mut lines = raw.lines();
            let first = lines.next().unwrap_or("");
            let path = first.split_whitespace().nth(1).unwrap_or("/");
            hits_ref.fetch_add(1, Ordering::SeqCst);
            let (status, headers, body) = handler(&raw, path);
            let reason = match status {
                200 => "OK",
                302 => "Found",
                307 => "Temporary Redirect",
                404 => "Not Found",
                _ => "Error",
            };
            let mut head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    (addr, hits, handle)
}

pub(crate) fn install_indexer_plugin(
    state: &ApiState,
    id: &str,
    http_origins: Vec<serde_json::Value>,
    grants: &[PluginGrant],
) -> PluginRecord {
    let rel = format!("plugins/pkg/{id}/v1/indexer.wasm");
    let path = state.store().data_dir().join(&rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, INDEXER_WASM).unwrap();
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "id": id,
        "name": "Fixture Indexer",
        "version": "1.0.0",
        "api": "1.0.0",
        "license": "MIT",
        "authors": ["Test"],
        "description": "Fixture",
        "config_schema": "config.schema.json",
        "state_schema": 1,
        "http": http_origins,
        "components": [{
            "id": "fixture",
            "entry": "indexer.wasm",
            "world": "crawler:plugin/indexer-plugin@1.0.0",
            "permissions": ["http", "http:site-auth", "read:sites", "indexer:provide"],
            "indexers": [{
                "id": "fixture",
                "name": "Fixture",
                "kind": "nexusphp",
                "capabilities": ["search"],
                "site_schema": "1"
            }]
        }]
    });
    let meta = PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: format!("{id}-hash-1"),
        version: "1.0.0".into(),
        manifest_json: serde_json::to_string(&manifest).unwrap(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: rel,
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: false,
    };
    let installed = state.store().register_plugin_version(&meta, 0).unwrap();
    let granted = state
        .store()
        .replace_plugin_grants(
            &installed.plugin_id,
            &installed.package_hash,
            installed.revision,
            grants,
        )
        .unwrap();
    enable(state.store().as_ref(), &granted)
}

fn make_site(
    id: SiteId,
    url: &str,
    cookie: Option<&str>,
    api_key: Option<&str>,
    proxy: Option<&str>,
) -> Site {
    Site {
        id,
        name: format!("site-{id}"),
        url: url.into(),
        profile_id: "mteam".into(),
        cookie: cookie.map(str::to_string),
        api_key: api_key.map(str::to_string),
        rss_url: None,
        proxy: proxy.map(str::to_string),
        rate_limit_per_minute: None,
        cdp_url: Some("ws://127.0.0.1:9222/devtools/browser/secret".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

pub(crate) fn grant_all(scope: PluginGrantScope) -> Vec<PluginGrant> {
    ["http", "http:site-auth", "read:sites", "indexer:provide"]
        .into_iter()
        .map(|cap| PluginGrant {
            capability: cap.into(),
            scope: scope.clone(),
            sensitive: cap == "http:site-auth",
        })
        .collect()
}

fn test_state_with_policy(
    dir: &std::path::Path,
    self_listen: SocketAddr,
    policy: TargetPolicy,
) -> ApiState {
    build_state(
        dir,
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    )
    .with_self_listen(self_listen)
    .with_plugin_http_policy_override(policy)
}

#[test]
fn public_url_enforces_manifest_self_listen_and_metadata_ip() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, hits, _srv) = spawn_http_server(|_raw, path| {
        if path.starts_with("/api/") {
            (
                200,
                vec![("Set-Cookie".into(), "leaked=1".into())],
                br#"ok-public"#.to_vec(),
            )
        } else {
            (200, vec![], b"other".to_vec())
        }
    });
    let self_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let self_addr = self_listener.local_addr().unwrap();

    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr)
        .with_test_loopback_socket(self_addr);
    let state = test_state_with_policy(tmp.path(), self_addr, policy);
    let origin = format!("http://{addr}");
    let self_origin = format!("http://{self_addr}");
    let plugin = install_indexer_plugin(
        &state,
        "org.example.http-public",
        vec![
            serde_json::json!({"origin": origin, "path_prefix": "/api", "methods": ["GET"]}),
            serde_json::json!({"origin": self_origin, "path_prefix": "/", "methods": ["GET"]}),
        ],
        &grant_all(PluginGrantScope::Global),
    );
    let ports = state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    let _g = ports.begin_invocation(&inv).unwrap();

    // Allowed public URL succeeds and strips Set-Cookie
    let ok = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl(format!("http://{addr}/api/items")),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, b"ok-public");
    assert!(
        ok.headers
            .iter()
            .all(|h| !h.name.eq_ignore_ascii_case("set-cookie"))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Path prefix mismatch rejected against manifest.http
    let bad_path = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl(format!("http://{addr}/admin")),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(bad_path.code, ErrorCode::PermissionDenied);

    // Method mismatch rejected against manifest.http
    let bad_method = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "POST".into(),
                target: HttpTarget::PublicUrl(format!("http://{addr}/api/items")),
                headers: vec![],
                body: Some(b"x".to_vec()),
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(bad_method.code, ErrorCode::PermissionDenied);

    // self_listen is unconditionally blocked even when manifest + test_loopback allow it
    let blocked_self = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl(format!("http://{self_addr}/health")),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(blocked_self.code, ErrorCode::PermissionDenied);
    assert_eq!(blocked_self.detail, Some(ErrorDetail::BlockedAddress));

    // Cloud metadata IP is blocked
    let metadata_err = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl("http://169.254.169.254/latest/meta-data/".into()),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(metadata_err.code, ErrorCode::PermissionDenied);
}

#[test]
fn wasm_indexer_site_relative_auth_injects_slot_and_strips_set_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    let seen_cookie = Arc::new(parking_lot::Mutex::new(String::new()));
    let seen_key = Arc::new(parking_lot::Mutex::new(String::new()));
    let seen_cookie_srv = seen_cookie.clone();
    let seen_key_srv = seen_key.clone();
    let (addr, _hits, _srv) = spawn_http_server(move |raw, path| {
        for line in raw.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("cookie:") {
                *seen_cookie_srv.lock() = line[line.len() - v.len()..].trim().to_string();
            }
            if let Some(v) = lower.strip_prefix("x-api-key:") {
                *seen_key_srv.lock() = line[line.len() - v.len()..].trim().to_string();
            }
        }
        assert!(
            path.starts_with("/api/search?q=matrix"),
            "unexpected path {path}"
        );
        (
            200,
            vec![("Set-Cookie".into(), "c_secure_uid=fresh99; HttpOnly".into())],
            b"ok-from-pt".to_vec(),
        )
    });

    let self_addr: SocketAddr = "127.0.0.1:19999".parse().unwrap();
    let state = test_state_with_policy(
        tmp.path(),
        self_addr,
        TargetPolicy::production().with_test_loopback_socket(addr),
    );
    let site_id = SiteId::new();
    let site = make_site(
        site_id,
        &format!("http://{addr}"),
        Some("c_secure_uid=secret-cookie"),
        Some("secret-api-key"),
        None,
    );
    state.store().insert_site(&site).unwrap();

    let plugin = install_indexer_plugin(
        &state,
        "org.example.site-auth",
        vec![],
        &grant_all(PluginGrantScope::Site(site_id)),
    );
    let host = state.plugins().wasm_host().unwrap().clone();
    let compiled = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();
    let provider = WasmIndexerProvider::new(
        host,
        compiled,
        WasmIndexerConfig {
            plugin_id: plugin.plugin_id.clone(),
            provider_id: "fixture".into(),
            descriptor: indexer::ProviderDescriptor {
                id: format!("plugin:{}/fixture", plugin.plugin_id),
                name: "Fixture".into(),
                kind: "nexusphp".into(),
                source: indexer::ProviderSource::PluginWasm {
                    plugin_id: plugin.plugin_id.clone(),
                    component_id: "fixture".into(),
                    version: "1.0.0".into(),
                    plugin_generation: u64::try_from(plugin.generation).unwrap(),
                },
                auth_types: vec![indexer::AuthType::Cookie],
                capabilities: vec![
                    indexer::ProviderCapability::Search,
                    indexer::ProviderCapability::Rss,
                    indexer::ProviderCapability::Detail,
                    indexer::ProviderCapability::ResolveDownload,
                    indexer::ProviderCapability::UserStats,
                    indexer::ProviderCapability::Categories,
                ],
                availability: indexer::ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            trust: plugin_api::PluginTrust::Untrusted,
            rate_limiter: None,
        },
    )
    .unwrap();

    struct NoopSiteFetch;
    impl SiteFetch for NoopSiteFetch {
        fn fetch(&self, _: &Site, _: &indexer::FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("unused".into()))
        }
    }
    let noop_fetch = NoopSiteFetch;
    let items = provider
        .search(
            &ProviderCallContext {
                generation: 99,
                fetcher: &noop_fetch,
            },
            &site,
            &indexer::SearchQuery {
                keyword: "site-auth-http",
                page: 1,
                categories: &[],
            },
        )
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "auth:ok-from-pt");
    assert_eq!(seen_cookie.lock().as_str(), "c_secure_uid=secret-cookie");
    assert_eq!(seen_key.lock().as_str(), "secret-api-key");
}

#[test]
fn site_port_strips_prefix_enforces_scope_redacts_secrets_and_rejects_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(
        tmp.path(),
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    );
    let site_a_id = SiteId::new();
    let site_b_id = SiteId::new();
    let site_a = make_site(
        site_a_id,
        "https://alpha.example/tracker",
        Some("alpha-cookie"),
        Some("alpha-key"),
        Some("http://proxy:pass@127.0.0.1:7890"),
    );
    let site_b = make_site(
        site_b_id,
        "https://beta.example/tracker",
        Some("beta-cookie"),
        None,
        None,
    );
    state.store().insert_site(&site_a).unwrap();
    state.store().insert_site(&site_b).unwrap();

    let plugin = install_indexer_plugin(
        &state,
        "org.example.site-scope",
        vec![],
        &grant_all(PluginGrantScope::Site(site_a_id)),
    );
    let ports = state.plugins().host_ports();
    let epoch = ports
        .site_credential_epoch(&format!("SITE:{site_a_id}"))
        .unwrap()
        .expect("epoch");

    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_a_id.to_string());
    inv.credential_epoch = Some(epoch);
    let _g = ports.begin_invocation(&inv).unwrap();

    // Case-insensitive "SITE:" prefix stripped cleanly; secrets redacted
    let view = ports.site_get(&inv, &format!("SITE:{site_a_id}")).unwrap();
    assert_eq!(view.id, site_a_id.to_string());
    let serialized = format!("{view:?}");
    assert!(!serialized.contains("alpha-cookie"));
    assert!(!serialized.contains("alpha-key"));
    assert!(!serialized.contains("7890"));
    assert!(!serialized.contains("alpha.example"));
    assert!(!serialized.contains("9222"));

    // Stats port works for bound site
    let stats = ports
        .site_stats(&inv, &format!("site:{site_a_id}"))
        .unwrap();
    assert!(stats.uploaded.is_none());

    // Cross-site access rejected
    let cross = ports.site_get(&inv, &site_b_id.to_string()).unwrap_err();
    assert_eq!(cross.code, ErrorCode::PermissionDenied);

    // Site.proxy causes UnsupportedRoute -> Unsupported on SiteRelative HTTP fetch
    let proxy_err = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::SiteRelative(SiteTarget {
                    path: "/torrents.php".into(),
                    query: vec![],
                    auth_slot: Some("site-cookie".into()),
                }),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(proxy_err.code, ErrorCode::Unsupported);

    // Site list with bound site only returns that site
    let page = ports
        .site_list(
            &inv,
            PageRequest {
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, site_a_id.to_string());
}

#[test]
fn host_ports_read_via_reader_pool_without_blocking_on_app_writer_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(
        tmp.path(),
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    );
    let site_id = SiteId::new();
    let site = make_site(site_id, "https://alpha.example", Some("c=1"), None, None);
    state.store().insert_site(&site).unwrap();
    let plugin = install_indexer_plugin(
        &state,
        "org.example.reader-pool",
        vec![],
        &grant_all(PluginGrantScope::Global),
    );
    let store = state.store().clone();
    let writer_entered = Arc::new(AtomicBool::new(false));
    let release_writer = Arc::new(AtomicBool::new(false));
    let entered_tx = writer_entered.clone();
    let release_rx = release_writer.clone();

    let writer_thread = std::thread::spawn(move || {
        store
            .app_write(|_app| {
                entered_tx.store(true, Ordering::Release);
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !release_rx.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            })
            .unwrap();
    });

    while !writer_entered.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(5));
    }

    let ports = state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(2));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_id.to_string());

    let started = std::time::Instant::now();
    let _g = ports.begin_invocation(&inv).unwrap();
    let view = ports.site_get(&inv, &site_id.to_string()).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "ReaderPool read blocked on App writer lock: {:?}",
        started.elapsed()
    );
    assert_eq!(view.id, site_id.to_string());

    release_writer.store(true, Ordering::Release);
    writer_thread.join().unwrap();
}

#[test]
fn site_relative_rejects_ipv4_mapped_loopback_and_unspecified_targeting_self_listen() {
    let listener = TcpListener::bind("[::]:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = hits.clone();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let srv = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !stop_srv.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            if let Ok((mut stream, _)) = listener.accept() {
                hits_srv.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret");
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let self_addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let state = build_state(
        tmp.path(),
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    )
    .with_self_listen(self_addr);
    let plugin = install_indexer_plugin(
        &state,
        "org.example.ssrf-v6",
        vec![],
        &grant_all(PluginGrantScope::Global),
    );
    let ports = state.plugins().host_ports();

    for host in ["[::ffff:127.0.0.1]", "[::ffff:0.0.0.0]", "0.0.0.0"] {
        let site_id = SiteId::new();
        let site = make_site(site_id, &format!("http://{host}:{port}"), None, None, None);
        state.store().insert_site(&site).unwrap();
        let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
        inv.generation = u64::try_from(plugin.generation).unwrap();
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(site_id.to_string());
        let _g = ports.begin_invocation(&inv).unwrap();
        let err = ports
            .http_fetch(
                &inv,
                HttpRequest {
                    method: "GET".into(),
                    target: HttpTarget::SiteRelative(SiteTarget {
                        path: "/api/v1/sites".into(),
                        query: vec![],
                        auth_slot: None,
                    }),
                    headers: vec![],
                    body: None,
                    max_bytes: 4096,
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied, "host {host}");
        assert_eq!(err.detail, Some(ErrorDetail::BlockedAddress), "host {host}");
    }
    stop.store(true, Ordering::Release);
    srv.join().unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn public_url_redirect_blocks_path_prefix_segment_boundary_and_307_method_escalation() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr_a, hits_a, _srv_a) = spawn_http_server(|_raw, path| {
        let loc = if path.starts_with("/api/go") {
            "/api-secret/dump"
        } else {
            "/unused"
        };
        (302, vec![("Location".into(), loc.into())], Vec::new())
    });
    let (addr_b, hits_b, _srv_b) = spawn_http_server(|raw, _path| {
        let method = raw.split_whitespace().next().unwrap_or("?");
        (200, vec![], format!("{method}:/admin").into_bytes())
    });
    let (addr_c, _hits_c, _srv_c) = spawn_http_server(move |_raw, _path| {
        (
            307,
            vec![("Location".into(), format!("http://{addr_b}/admin"))],
            Vec::new(),
        )
    });

    let self_addr: SocketAddr = "127.0.0.1:19998".parse().unwrap();
    let policy = TargetPolicy::production()
        .with_test_loopback_socket(addr_a)
        .with_test_loopback_socket(addr_b)
        .with_test_loopback_socket(addr_c);
    let state = test_state_with_policy(tmp.path(), self_addr, policy);
    let plugin = install_indexer_plugin(
        &state,
        "org.example.redir-guard",
        vec![
            serde_json::json!({"origin": format!("http://{addr_a}"), "path_prefix": "/api", "methods": ["GET"]}),
            serde_json::json!({"origin": format!("http://{addr_c}"), "path_prefix": "/go", "methods": ["POST"]}),
            serde_json::json!({"origin": format!("http://{addr_b}"), "path_prefix": "/admin", "methods": ["GET"]}),
        ],
        &grant_all(PluginGrantScope::Global),
    );
    let ports = state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    let _g = ports.begin_invocation(&inv).unwrap();

    let err_prefix = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "GET".into(),
                target: HttpTarget::PublicUrl(format!("http://{addr_a}/api/go")),
                headers: vec![],
                body: None,
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(err_prefix.code, ErrorCode::PermissionDenied);
    assert_eq!(err_prefix.detail, Some(ErrorDetail::BlockedAddress));
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);

    let err_307 = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "POST".into(),
                target: HttpTarget::PublicUrl(format!("http://{addr_c}/go")),
                headers: vec![],
                body: Some(b"k=1".to_vec()),
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(err_307.code, ErrorCode::UnknownOutcome);
    assert_eq!(hits_b.load(Ordering::SeqCst), 0);
}
