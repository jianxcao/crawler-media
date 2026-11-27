#[path = "site_maintenance_eof_challenge/command_outcomes.rs"]
mod command_outcomes;
#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[path = "site_maintenance_eof_challenge/status_redirects.rs"]
mod status_redirects;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::{MaintenanceVia, Site, SiteId};
use net_guard::TargetPolicy;
use plugin_api::{EndpointRequest, Header};
use plugin_http_support::{HttpEnv, TOKEN, assert_ok, req_json};
use serde_json::json;
use store::plugins::*;
use store::{SiteMaintenanceAction, SiteMaintenanceReceiptFilter, SiteMaintenanceState};

const MAINTENANCE_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/maintenance_fixture.wasm");
const ENDPOINT_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/endpoint_fixture.wasm");

enum StubBehavior {
    DropImmediately,
    PartialBody,
    ChallengeHtml,
    AuthExpiredHtml,
    SuccessHtml,
}

fn spawn_raw_stub(
    behaviors: Vec<StubBehavior>,
) -> (SocketAddr, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = hits.clone();
    let handle = std::thread::spawn(move || {
        for behavior in behaviors {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            hits_srv.fetch_add(1, Ordering::SeqCst);
            match behavior {
                StubBehavior::DropImmediately => {
                    drop(stream);
                }
                StubBehavior::PartialBody => {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nhi");
                    let _ = stream.flush();
                    drop(stream);
                }
                StubBehavior::ChallengeHtml => {
                    let b = "<div id=\"cf-challenge-running\">Checking your browser</div>";
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                        b.len()
                    );
                    let _ = stream.write_all(r.as_bytes());
                }
                StubBehavior::AuthExpiredHtml => {
                    let b = "<html><body><div>未登录</div></body></html>";
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                        b.len()
                    );
                    let _ = stream.write_all(r.as_bytes());
                }
                StubBehavior::SuccessHtml => {
                    let b = "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>";
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                        b.len()
                    );
                    let _ = stream.write_all(r.as_bytes());
                }
            }
        }
    });
    (addr, hits, handle)
}

async fn insert_nexus_site(env: &HttpEnv, url: &str) -> SiteId {
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "EofTestSite",
                "url": url,
                "profile_id": "pterclub",
                "cookie": "uid=1; pass=init",
                "maintenance_via": "http",
                "timezone": "UTC",
                "enabled": true,
            })),
        ))
        .await;
    let created = assert_ok(st, &body, StatusCode::CREATED);
    created["id"].as_str().unwrap().parse().unwrap()
}

fn latest_checkin_receipt_from_state(
    state: &api::ApiState,
    site_id: SiteId,
) -> store::SiteMaintenanceReceipt {
    let page = state
        .store()
        .list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter {
                site_id: Some(site_id),
                action: Some(SiteMaintenanceAction::CheckIn),
                state: None,
            },
            None,
            Some(10),
        )
        .unwrap();
    page.items.into_iter().next().expect("receipt")
}

fn latest_checkin_receipt(env: &HttpEnv, site_id: SiteId) -> store::SiteMaintenanceReceipt {
    latest_checkin_receipt_from_state(&env.state, site_id)
}

#[tokio::test]
async fn check_in_post_write_eof_and_partial_body_freeze_receipt_as_unknown() {
    for behavior in [StubBehavior::DropImmediately, StubBehavior::PartialBody] {
        let env = HttpEnv::new().await;
        let (addr, hits, _srv) = spawn_raw_stub(vec![behavior]);
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
        let r = latest_checkin_receipt(&env, site_id);
        assert_eq!(r.state, SiteMaintenanceState::Unknown);

        // Second check-in in same window must be blocked with 0 extra HTTP requests
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
}

#[tokio::test]
async fn check_in_pre_send_connect_refused_is_failed_and_retryable() {
    let env = HttpEnv::new().await;
    let dead_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead_listener.local_addr().unwrap();
    drop(dead_listener);

    let site_id = insert_nexus_site(&env, &format!("http://{dead_addr}/")).await;
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
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Failed
    );

    let (ok_addr, hits, srv) = spawn_raw_stub(vec![StubBehavior::SuccessHtml]);
    let (st_patch, _, _) = env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/sites/{site_id}"),
            Some(TOKEN),
            &[],
            Some(json!({ "url": format!("http://{ok_addr}/") })),
        ))
        .await;
    assert_eq!(st_patch, StatusCode::OK);

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
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Completed
    );
    srv.join().unwrap();
}

#[tokio::test]
async fn reject_challenge_records_challenge_outcome_vs_auth_expired() {
    let env = HttpEnv::new().await;
    let (addr, hits, srv) = spawn_raw_stub(vec![
        StubBehavior::ChallengeHtml,
        StubBehavior::AuthExpiredHtml,
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
    let r2 = latest_checkin_receipt(&env, site_id);
    assert_eq!(r2.state, SiteMaintenanceState::Failed);
    assert_eq!(r2.outcome.as_deref(), Some("auth-expired"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    srv.join().unwrap();
}

fn install_maint_plugin(state: &api::ApiState, id: &str, site_id: SiteId) -> PluginRecord {
    let rel = format!("plugins/pkg/{id}/v1/maint.wasm");
    let path = state.store().data_dir().join(&rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, MAINTENANCE_WASM).unwrap();
    let manifest = json!({
        "manifest_version": 1, "id": id, "name": "Maint EOF", "version": "1.0.0",
        "api": "1.0.0", "license": "MIT", "authors": ["Test"], "description": "EOF",
        "config_schema": "config.schema.json", "state_schema": 1,
        "credential_binding": "invocation-site",
        "components": [{
            "id": "maint", "entry": "maint.wasm",
            "world": "crawler:plugin/maintenance-plugin@1.0.0",
            "permissions": ["http", "http:site-auth", "read:sites", "maintenance:provide"],
            "maintenance": [{"id": "maint", "actions": ["verify-session", "check-in"]}]
        }]
    });
    let meta = PluginVersionMetadata {
        plugin_id: id.into(),
        package_hash: format!("{id}-h1"),
        version: "1.0.0".into(),
        manifest_json: serde_json::to_string(&manifest).unwrap(),
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
        scope: PluginGrantScope::Site(site_id),
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

#[derive(Default)]
struct OfflineFetcherForTest;
impl indexer::Fetcher for OfflineFetcherForTest {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

#[test]
fn wasm_maintenance_post_write_eof_freezes_receipt_as_unknown() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let (addr, hits, _srv) = spawn_raw_stub(vec![StubBehavior::DropImmediately]);
    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let fetcher = Arc::new(OfflineFetcherForTest);
    let policy = TargetPolicy::production().with_test_loopback_socket(addr);
    let state = api::ApiState::new_arc(
        store,
        TOKEN.into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl,
    )
    .unwrap()
    .with_plugin_http_policy_override(policy);

    let site_id = SiteId::new();
    let site = Site {
        id: site_id,
        name: "WasmMaintEof".into(),
        url: format!("http://{addr}"),
        profile_id: "demo".into(),
        cookie: Some("uid=1".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    };
    state.store().insert_site(&site).unwrap();
    install_maint_plugin(&state, "org.example.maint-eof", site_id);
    api::plugins::sync_registry_snapshot(&state, true).unwrap();

    let obs = state.store().observe_site(site_id).unwrap().unwrap();
    let provider_id = "plugin:org.example.maint-eof/maint";
    let Err(err1) = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_800,
        "http-checkin",
    ) else {
        panic!("expected Err for post-write EOF");
    };
    assert!(matches!(
        err1,
        api::site_maintenance::MaintenanceError::Unknown(_)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let Err(err2) = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_805,
        "http-checkin",
    ) else {
        panic!("expected BlockedUnknown");
    };
    assert!(matches!(
        err2,
        api::site_maintenance::MaintenanceError::BlockedUnknown(_)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

struct LoopbackGetFetcher;
impl indexer::Fetcher for LoopbackGetFetcher {
    fn fetch(&self, request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        let parsed = url::Url::parse(&request.url)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port_or_known_default().unwrap_or(80);
        let path = parsed.path();
        let mut stream = std::net::TcpStream::connect((host, port))
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        let mut buf = String::new();
        stream
            .read_to_string(&mut buf)
            .map_err(|e| indexer::IndexerError::Fetch(e.to_string()))?;
        Ok(buf
            .split_once("\r\n\r\n")
            .map_or(buf.as_str(), |(_, b)| b)
            .to_string())
    }
}

async fn live_http_env_for_eof() -> HttpEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage")));
    let state = api::ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(plugin_http_support::ADMIN_PASS.into()),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(LoopbackGetFetcher),
        dl,
    )
    .unwrap();
    let jobs = state.jobs();
    let queue = jobs.acquire().unwrap();
    for def in queue.list_defs().unwrap() {
        if !def.kind.is_plugin() {
            let _ = queue.set_def_enabled_by_id(def.id, false);
        }
    }
    drop(queue);
    HttpEnv {
        _dir: dir,
        app: api::router(state.clone()),
        state,
        member_token: String::new(),
    }
}

fn enable_ep_for_site(env: &HttpEnv, site_id: SiteId) -> PluginRecord {
    let rel = "plugins/pkg/org.example.ep-maint/v1/ep.wasm";
    let path = env.state.store().data_dir().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, ENDPOINT_WASM).unwrap();
    let meta = PluginVersionMetadata {
        plugin_id: "org.example.ep-maint".into(),
        package_hash: "ep-maint-h1".into(),
        version: "1.0.0".into(),
        manifest_json: "{}".into(),
        config_schema_json: "{}".into(),
        state_schema: "1".into(),
        artifact_path: rel.into(),
        signing_key_id: None,
        signature_status: "development".into(),
        ui_metadata_ref: None,
        credential_bound: true,
    };
    let p = env.state.store().register_plugin_version(&meta, 0).unwrap();
    let grants = vec![
        PluginGrant {
            capability: "kv".into(),
            scope: PluginGrantScope::Site(site_id),
            sensitive: false,
        },
        PluginGrant {
            capability: "command:site.verify-session".into(),
            scope: PluginGrantScope::Site(site_id),
            sensitive: false,
        },
    ];
    let p = env
        .state
        .store()
        .replace_plugin_grants(&p.plugin_id, &p.package_hash, p.revision, &grants)
        .unwrap();
    let snap = env
        .state
        .store()
        .begin_plugin_lifecycle(&p.plugin_id, p.revision, uuid::Uuid::new_v4(), true)
        .unwrap();
    env.state
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

#[tokio::test]
async fn guest_site_maintenance_command_executes_end_to_end_via_jobs_tick() {
    let env = live_http_env_for_eof().await;
    let (_addr, hits, _srv) = spawn_raw_stub(vec![StubBehavior::SuccessHtml]);
    let site_id = insert_nexus_site(&env, &format!("http://{_addr}/")).await;
    let enabled = enable_ep_for_site(&env, site_id);

    let obs = env.state.store().observe_site(site_id).unwrap().unwrap();
    let auth = env
        .state
        .store()
        .issue_plugin_state_authority(&enabled, Some(&obs))
        .unwrap();
    let subject = PluginSubject::User(plugin_http_support::admin_id());
    let resp = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth,
            subject.clone(),
            PluginCommandCause::root("ep-maint-cmd"),
            EndpointRequest {
                method: "POST".into(),
                path: "/submit".into(),
                query: Vec::<Header>::new(),
                body: b"guest-maint-1".to_vec(),
            },
        )
        .unwrap();
    assert_eq!(resp.status, 202);
    let op_id: uuid::Uuid = std::str::from_utf8(&resp.body).unwrap().parse().unwrap();

    let (st_tick, _, body_tick) = env
        .send(req_json(
            "POST",
            "/api/v1/jobs/tick?now=1795276800",
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st_tick, &body_tick, StatusCode::OK);

    let op = env
        .state
        .store()
        .get_plugin_operation(&subject, op_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, PluginOperationState::Succeeded);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let page = env
        .state
        .store()
        .list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter {
                site_id: Some(site_id),
                action: Some(SiteMaintenanceAction::VerifySession),
                state: None,
            },
            None,
            Some(10),
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].state, SiteMaintenanceState::Completed);
}
