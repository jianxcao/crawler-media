use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::SiteId;
use plugin_api::{ErrorCode, ErrorDetail, GuestRole, HttpRequest, HttpTarget, SiteTarget};
use plugin_host::Invocation;
use serde_json::json;
use store::SiteMaintenanceState;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycleObservation, PluginRecord, PluginVersionMetadata,
};

use super::plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use super::{insert_nexus_site, latest_checkin_receipt};

fn spawn_sequence_server(
    responses: Vec<(u16, &'static str, &'static str)>,
) -> (SocketAddr, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);
    let handle = std::thread::spawn(move || {
        for (status, reason, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            hits_srv.fetch_add(1, Ordering::SeqCst);
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    (addr, hits, handle)
}

#[tokio::test]
async fn check_in_hop0_post_500_freezes_receipt_unknown_and_blocks_duplicate_post() {
    let env = HttpEnv::new().await;
    let (addr, hits, _srv) = spawn_sequence_server(vec![(500, "Internal Server Error", "oops")]);
    let site_id = insert_nexus_site(&env, &format!("http://{addr}/")).await;

    let (st1, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st1, StatusCode::BAD_REQUEST);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Unknown
    );

    let (st2, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st2, StatusCode::BAD_REQUEST);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn check_in_hop0_post_403_cf_challenge_records_challenge_and_allows_retry_while_plain_503_freezes_unknown()
 {
    let env = HttpEnv::new().await;
    let (addr, hits, srv) = spawn_sequence_server(vec![
        (
            403,
            "Forbidden",
            r#"<div id="cf-challenge-running">Checking your browser</div>"#,
        ),
        (
            200,
            "OK",
            r#"<html><body><p>签到成功</p><a href="logout.php">Logout</a></body></html>"#,
        ),
    ]);
    let site_id = insert_nexus_site(&env, &format!("http://{addr}/")).await;

    let (st1, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st1, StatusCode::BAD_REQUEST);
    let r1 = latest_checkin_receipt(&env, site_id);
    assert_eq!(r1.state, SiteMaintenanceState::Failed);
    assert_eq!(r1.outcome.as_deref(), Some("challenge"));

    let (st2, _, body2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st2, &body2, StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Completed
    );
    srv.join().unwrap();

    let (addr_503, hits_503, _srv_503) = spawn_sequence_server(vec![(
        503,
        "Service Unavailable",
        "Maintenance in progress",
    )]);
    let site_503 = insert_nexus_site(&env, &format!("http://{addr_503}/")).await;
    let (st3, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_503}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st3, StatusCode::BAD_REQUEST);
    assert_eq!(
        latest_checkin_receipt(&env, site_503).state,
        SiteMaintenanceState::Unknown
    );
    let (st4, _, _) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_503}/check-in"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_eq!(st4, StatusCode::BAD_REQUEST);
    assert_eq!(hits_503.load(Ordering::SeqCst), 1);
}

fn spawn_stop_server(
    handler: impl Fn(&mut std::net::TcpStream) + Send + 'static,
) -> (SocketAddr, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = Arc::clone(&stop);
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Acquire) {
            if let Ok((mut s, _)) = listener.accept() {
                handler(&mut s);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    (addr, stop, handle)
}

fn enable_global_http_maint_plugin(state: &api::ApiState, id: &str) -> PluginRecord {
    let rel = format!("plugins/pkg/{id}/v1/maint.wasm");
    let path = state.store().data_dir().join(&rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, super::MAINTENANCE_WASM).unwrap();
    let meta = PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: format!("{id}-h1"),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: rel,
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: true,
    };
    let p = state.store().register_plugin_version(&meta, 0).unwrap();
    let grants: Vec<_> = [
        "http",
        "http:site-auth",
        "read:sites",
        "maintenance:provide",
    ]
    .into_iter()
    .map(|cap| PluginGrant {
        capability: cap.into(),
        scope: PluginGrantScope::Global,
        sensitive: cap == "http:site-auth",
    })
    .collect();
    let p = state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let snap = state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    state
        .store()
        .finalize_plugin_lifecycle(&PluginLifecycleObservation {
            plugin_id: snap.plugin_id,
            revision: snap.revision,
            generation: snap.generation,
            operation_id: snap.active_operation_id.unwrap(),
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

fn insert_http_test_site(state: &api::ApiState, id: SiteId, url: String) {
    state
        .store()
        .insert_site(&domain::Site {
            id,
            name: format!("Site-{id}"),
            url,
            profile_id: "demo".into(),
            cookie: Some("uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
}

#[test]
fn store_host_ports_post_302_self_listen_returns_unknown_outcome_while_hop0_returns_permission_denied()
 {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let self_hits = Arc::new(AtomicUsize::new(0));
    let self_hits_srv = Arc::clone(&self_hits);
    let (self_addr, stop_self, self_srv) = spawn_stop_server(move |s| {
        self_hits_srv.fetch_add(1, Ordering::SeqCst);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    });
    let (site_addr, stop_site, site_srv) = spawn_stop_server(move |s| {
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{self_addr}/admin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = s.write_all(resp.as_bytes());
        let _ = s.flush();
    });

    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let fetcher = Arc::new(super::OfflineFetcherForTest);
    let policy = net_guard::TargetPolicy::production().with_test_loopback_socket(site_addr);
    let state = api::ApiState::new_arc(
        store,
        TOKEN.into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl,
    )
    .unwrap()
    .with_self_listen(self_addr)
    .with_plugin_http_policy_override(policy);

    let (site_ok, site_self) = (SiteId::new(), SiteId::new());
    insert_http_test_site(&state, site_ok, format!("http://{site_addr}"));
    insert_http_test_site(&state, site_self, format!("http://{self_addr}"));
    let enabled = enable_global_http_maint_plugin(&state, "org.example.http-ports-302");

    let ports = state.plugins().host_ports();
    let mut inv_302 = Invocation::new(enabled.plugin_id.clone(), Duration::from_secs(5));
    inv_302.generation = u64::try_from(enabled.generation).unwrap();
    inv_302.role = GuestRole::Maintenance;
    inv_302.site_id = Some(site_ok.to_string());
    let _g1 = ports.begin_invocation(&inv_302).unwrap();
    let req_302 = HttpRequest {
        method: "POST".into(),
        target: HttpTarget::SiteRelative(SiteTarget {
            path: "/attendance.php".into(),
            query: vec![],
            auth_slot: None,
        }),
        headers: vec![],
        body: Some(b"ok=1".to_vec()),
        max_bytes: 4096,
    };
    let err_302 = ports.http_fetch(&inv_302, req_302.clone()).unwrap_err();
    assert_eq!(err_302.code, ErrorCode::UnknownOutcome);
    assert_eq!(err_302.detail, Some(ErrorDetail::BlockedAddress));

    let err_many = ports
        .http_fetch_many(&inv_302, vec![req_302])
        .into_iter()
        .next()
        .unwrap()
        .unwrap_err();
    assert_eq!(err_many.code, ErrorCode::UnknownOutcome);
    assert_eq!(err_many.detail, Some(ErrorDetail::BlockedAddress));

    let mut inv_hop0 = Invocation::new(enabled.plugin_id, Duration::from_secs(5));
    inv_hop0.generation = u64::try_from(enabled.generation).unwrap();
    inv_hop0.role = GuestRole::Maintenance;
    inv_hop0.site_id = Some(site_self.to_string());
    let _g2 = ports.begin_invocation(&inv_hop0).unwrap();
    let err_hop0 = ports
        .http_fetch(
            &inv_hop0,
            HttpRequest {
                method: "POST".into(),
                target: HttpTarget::SiteRelative(SiteTarget {
                    path: "/attendance.php".into(),
                    query: vec![],
                    auth_slot: None,
                }),
                headers: vec![],
                body: Some(b"ok=1".to_vec()),
                max_bytes: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(err_hop0.code, ErrorCode::PermissionDenied);
    assert_eq!(err_hop0.detail, Some(ErrorDetail::BlockedAddress));
    assert_eq!(self_hits.load(Ordering::SeqCst), 0);

    stop_site.store(true, Ordering::Release);
    stop_self.store(true, Ordering::Release);
    site_srv.join().unwrap();
    self_srv.join().unwrap();
}
