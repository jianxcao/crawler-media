use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use plugin_api::{ErrorCode, ErrorDetail, GuestRole, HttpRequest, HttpTarget};
use plugin_host::Invocation;
use store::plugins::PluginGrantScope;

use super::support::{FakeEffectMode, FakeEffects, build_state};
use super::{grant_all, install_indexer_plugin, spawn_http_server};

#[test]
fn public_url_resolves_relative_path_and_allows_declared_loopback_without_policy_override() {
    let tmp = tempfile::tempdir().unwrap();
    let (webhook_addr, webhook_hits, _srv_webhook) = spawn_http_server(|raw, path| {
        if raw.starts_with("POST /webhook/events ") || path == "/webhook/events" {
            (200, vec![], br#"{"ok":true}"#.to_vec())
        } else {
            (404, vec![], b"not-found".to_vec())
        }
    });
    let self_addr: SocketAddr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let state = build_state(
        tmp.path(),
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    )
    .with_self_listen(self_addr);

    let plugin = install_indexer_plugin(
        &state,
        "org.example.http-relative-webhook",
        vec![serde_json::json!({
            "origin": format!("http://{webhook_addr}"),
            "path_prefix": "/webhook",
            "methods": ["POST"]
        })],
        &grant_all(PluginGrantScope::Global),
    );
    let ports = state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    let _g = ports.begin_invocation(&inv).unwrap();

    let ok = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "POST".into(),
                target: HttpTarget::PublicUrl("/webhook/events".into()),
                headers: vec![],
                body: Some(br#"{"id":"ev-1"}"#.to_vec()),
                max_bytes: 4096,
            },
        )
        .unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, br#"{"ok":true}"#);
    assert_eq!(webhook_hits.load(Ordering::SeqCst), 1);

    let wrong_path = ports
        .http_fetch(
            &inv,
            HttpRequest {
                method: "POST".into(),
                target: HttpTarget::PublicUrl("/other".into()),
                headers: vec![],
                body: Some(b"{}".to_vec()),
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(wrong_path.code, ErrorCode::PermissionDenied);
}

#[test]
fn public_url_without_override_still_blocks_undeclared_private_unspecified_self_and_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let (undeclared_addr, undeclared_hits, _srv_undeclared) =
        spawn_http_server(|_raw, _path| (200, vec![], b"leak".to_vec()));
    let self_addr: SocketAddr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let state = build_state(
        tmp.path(),
        Arc::new(FakeEffects::new(FakeEffectMode::Succeed)),
        None,
    )
    .with_self_listen(self_addr);

    let plugin = install_indexer_plugin(
        &state,
        "org.example.http-ssrf-no-override",
        vec![
            serde_json::json!({"origin": format!("http://{self_addr}"), "path_prefix": "/", "methods": ["POST"]}),
            serde_json::json!({"origin": "http://0.0.0.0:18080", "path_prefix": "/", "methods": ["POST"]}),
        ],
        &grant_all(PluginGrantScope::Global),
    );
    let ports = state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Indexer;
    let _g = ports.begin_invocation(&inv).unwrap();

    let post_req = |url: String| HttpRequest {
        method: "POST".into(),
        target: HttpTarget::PublicUrl(url),
        headers: vec![],
        body: Some(b"{}".to_vec()),
        max_bytes: 4096,
    };

    assert_ne!(
        ports
            .http_fetch(
                &inv,
                post_req(format!("//{undeclared_addr}/webhook/events"))
            )
            .unwrap_err()
            .code,
        ErrorCode::Upstream
    );
    assert_eq!(
        ports
            .http_fetch(
                &inv,
                post_req(format!("http://{undeclared_addr}/webhook/events"))
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(undeclared_hits.load(Ordering::SeqCst), 0);

    let unspecified = ports
        .http_fetch(&inv, post_req("http://0.0.0.0:18080/hook".into()))
        .unwrap_err();
    assert_eq!(unspecified.code, ErrorCode::PermissionDenied);
    assert_eq!(unspecified.detail, Some(ErrorDetail::BlockedAddress));

    let self_err = ports
        .http_fetch(&inv, post_req(format!("http://{self_addr}/webhook")))
        .unwrap_err();
    assert_eq!(self_err.code, ErrorCode::PermissionDenied);
    assert_eq!(self_err.detail, Some(ErrorDetail::BlockedAddress));

    let meta_err = ports
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
    assert_eq!(meta_err.code, ErrorCode::PermissionDenied);
}
