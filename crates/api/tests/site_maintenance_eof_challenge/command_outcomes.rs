use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::plugin_http_support::{HttpEnv, TOKEN, assert_ok, etag, req_json};
use axum::http::StatusCode;
use domain::{MaintenanceVia, PluginOperationId, Site, SiteId};
use jobs::{JobIdentity, JobKind, JobScope};
use plugin_api::{CommandAction, CommandRequest, GuestRole, MaintenanceAction};
use plugin_host::Invocation;
use serde_json::json;
use store::SiteMaintenanceState;
use store::plugins::{
    PluginGrant, PluginGrantScope, PluginLifecycleObservation, PluginOperationState, PluginRecord,
    PluginVersionMetadata,
};

use super::{
    ENDPOINT_WASM, StubBehavior, insert_nexus_site, latest_checkin_receipt, spawn_raw_stub,
};

fn enable_guest_maint_plugin_for_sites(env: &HttpEnv, site_ids: &[SiteId]) -> PluginRecord {
    let rel = "plugins/pkg/org.example.ep-maint-cmd/v1/ep.wasm";
    let path = env.state.store().data_dir().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, ENDPOINT_WASM).unwrap();
    let meta = PluginVersionMetadata {
        plugin_id: "org.example.ep-maint-cmd".into(),
        package_hash: "ep-maint-cmd-h1".into(),
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
    let grants: Vec<_> = site_ids
        .iter()
        .flat_map(|&site_id| {
            ["command:site.check-in", "command:site.verify-session"]
                .into_iter()
                .map(move |cap| PluginGrant {
                    capability: cap.into(),
                    scope: PluginGrantScope::Site(site_id),
                    sensitive: false,
                })
        })
        .collect();
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

fn submit_guest_site_maintenance(
    env: &HttpEnv,
    plugin: &PluginRecord,
    site_id: SiteId,
    action: MaintenanceAction,
    idem_key: &str,
) -> uuid::Uuid {
    let ports = env.state.plugins().host_ports();
    let mut inv = Invocation::new(plugin.plugin_id.clone(), Duration::from_secs(5));
    inv.generation = u64::try_from(plugin.generation).unwrap();
    inv.role = GuestRole::Endpoint;
    inv.site_id = Some(format!("site:{site_id}"));
    let _guard = ports.begin_invocation(&inv).unwrap();
    let op_id = ports
        .commands_submit(
            &inv,
            CommandRequest {
                action: CommandAction::SiteMaintenance {
                    site_handle: format!("site:{site_id}"),
                    action,
                },
                idempotency_key: idem_key.into(),
                expected_revision: None,
            },
        )
        .unwrap();
    op_id.parse().unwrap()
}

async fn tick_jobs(env: &HttpEnv, now: i64) {
    let (st, _, body) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/jobs/tick?now={now}"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    assert_ok(st, &body, StatusCode::OK);
}

#[tokio::test]
async fn plugin_actions_checkin_eof_reaches_unknown_and_schedules_reconcile_without_duplicate_post()
{
    let env = HttpEnv::new().await;
    let (addr, hits, _srv) = spawn_raw_stub(vec![StubBehavior::DropImmediately]);
    let site_id = insert_nexus_site(&env, &format!("http://{addr}/")).await;

    let (st_status, hdrs_status, body_status) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/sites/{site_id}/plugin-status"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_status, &body_status, StatusCode::OK);
    let site_etag = etag(&hdrs_status);

    let (st1, _, body1) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/plugin-actions"),
            Some(TOKEN),
            &[("if-match", &site_etag), ("idempotency-key", "pa-eof-1")],
            Some(json!({ "action": "checkin" })),
        ))
        .await;
    let op1 = assert_ok(st1, &body1, StatusCode::ACCEPTED);
    let op1_id: uuid::Uuid = op1["operation_id"].as_str().unwrap().parse().unwrap();

    tick_jobs(&env, 1_795_276_800).await;

    let (st_op1, _, body_op1) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{op1_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let view1 = assert_ok(st_op1, &body_op1, StatusCode::OK);
    assert_eq!(view1["state"], "unknown");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Unknown
    );

    let reconcile_jobs = env
        .state
        .jobs()
        .acquire()
        .unwrap()
        .jobs_for_identity(
            JobIdentity {
                kind: JobKind::PluginReconcile,
                scope: JobScope::PluginOperation(PluginOperationId::from_uuid(op1_id)),
            },
            10,
        )
        .unwrap();
    assert!(
        !reconcile_jobs.is_empty(),
        "unknown plugin-actions operation must schedule PluginReconcile job"
    );

    let (st2, _, body2) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/plugin-actions"),
            Some(TOKEN),
            &[("if-match", &site_etag), ("idempotency-key", "pa-eof-2")],
            Some(json!({ "action": "checkin" })),
        ))
        .await;
    let op2 = assert_ok(st2, &body2, StatusCode::ACCEPTED);
    let op2_id: uuid::Uuid = op2["operation_id"].as_str().unwrap().parse().unwrap();
    tick_jobs(&env, 1_795_276_801).await;

    let (st_op2, _, body_op2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugin-operations/{op2_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let view2 = assert_ok(st_op2, &body_op2, StatusCode::OK);
    assert_eq!(view2["state"], "unknown");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn guest_site_maintenance_checkin_eof_transitions_to_unknown() {
    let env = HttpEnv::new().await;
    let (addr, hits, _srv) = spawn_raw_stub(vec![StubBehavior::DropImmediately]);
    let site_id = insert_nexus_site(&env, &format!("http://{addr}/")).await;
    let plugin = enable_guest_maint_plugin_for_sites(&env, &[site_id]);

    let op_id = submit_guest_site_maintenance(
        &env,
        &plugin,
        site_id,
        MaintenanceAction::CheckIn,
        "guest-checkin-eof-1",
    );
    tick_jobs(&env, 1_795_276_802).await;

    let pending = env
        .state
        .store()
        .plugin_command_status(op_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.operation.state,
        PluginOperationState::Unknown,
        "guest SiteMaintenance CheckIn EOF must transition to Unknown, got {:?} ({})",
        pending.operation.state,
        pending.operation.phase
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        latest_checkin_receipt(&env, site_id).state,
        SiteMaintenanceState::Unknown
    );
}

#[tokio::test]
async fn guest_site_maintenance_non_success_failures_transition_to_failed_or_rejected() {
    let env = HttpEnv::new().await;
    let (addr, hits, srv) = spawn_raw_stub(vec![StubBehavior::ChallengeHtml]);
    let challenge_site = insert_nexus_site(&env, &format!("http://{addr}/")).await;

    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let refused_site = insert_nexus_site(&env, &format!("http://{dead_addr}/")).await;

    let bad_tz_site = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id: bad_tz_site,
            name: "BadTzGuest".into(),
            url: "http://127.0.0.1:9/".into(),
            profile_id: "pterclub".into(),
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
            timezone: "Mars/Olympus_Mons".into(),
        })
        .unwrap();

    let plugin =
        enable_guest_maint_plugin_for_sites(&env, &[challenge_site, refused_site, bad_tz_site]);

    // 1. Challenge -> Failed
    let op_challenge = submit_guest_site_maintenance(
        &env,
        &plugin,
        challenge_site,
        MaintenanceAction::CheckIn,
        "guest-challenge-1",
    );
    tick_jobs(&env, 1_795_276_803).await;
    let op_c = env
        .state
        .store()
        .plugin_command_status(op_challenge)
        .unwrap()
        .unwrap()
        .operation;
    assert_eq!(op_c.state, PluginOperationState::Failed);
    assert_eq!(op_c.phase, "maintenance-failed");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let r_c = latest_checkin_receipt(&env, challenge_site);
    assert_eq!(r_c.state, SiteMaintenanceState::Failed);
    assert_eq!(r_c.outcome.as_deref(), Some("challenge"));
    srv.join().unwrap();

    // 2. ConnectRefused -> Failed
    let op_refused = submit_guest_site_maintenance(
        &env,
        &plugin,
        refused_site,
        MaintenanceAction::CheckIn,
        "guest-refused-1",
    );
    tick_jobs(&env, 1_795_276_804).await;
    let op_r = env
        .state
        .store()
        .plugin_command_status(op_refused)
        .unwrap()
        .unwrap()
        .operation;
    assert_eq!(op_r.state, PluginOperationState::Failed);
    assert_eq!(op_r.phase, "maintenance-failed");
    assert_eq!(
        latest_checkin_receipt(&env, refused_site).state,
        SiteMaintenanceState::Failed
    );

    // 3. InvalidTimezone -> Rejected
    let op_tz = submit_guest_site_maintenance(
        &env,
        &plugin,
        bad_tz_site,
        MaintenanceAction::CheckIn,
        "guest-badtz-1",
    );
    tick_jobs(&env, 1_795_276_805).await;
    let op_tz_res = env
        .state
        .store()
        .plugin_command_status(op_tz)
        .unwrap()
        .unwrap()
        .operation;
    assert_eq!(op_tz_res.state, PluginOperationState::Rejected);
    assert_eq!(op_tz_res.phase, "invalid-timezone");
}

#[test]
fn wasm_maintenance_post_302_same_origin_connection_refused_freezes_receipt_unknown() {
    use std::io::{Read as _, Write as _};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let post_hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&post_hits);
    let srv = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        hits_srv.fetch_add(1, Ordering::SeqCst);
        // Drop listener before sending 302 so same-origin GET /done gets ConnectionRefused.
        drop(listener);
        let _ = stream.write_all(
            b"HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let _ = stream.flush();
        drop(stream);
    });

    let store = Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let fetcher = Arc::new(super::OfflineFetcherForTest);
    let policy = net_guard::TargetPolicy::production().with_test_loopback_socket(addr);
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
    state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "WasmMaint302Refused".into(),
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
        })
        .unwrap();
    super::install_maint_plugin(&state, "org.example.maint-302", site_id);
    api::plugins::sync_registry_snapshot(&state, true).unwrap();

    let obs = state.store().observe_site(site_id).unwrap().unwrap();
    let provider_id = "plugin:org.example.maint-302/maint";
    let err1 = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_800,
        "http-checkin",
    )
    .err()
    .expect("POST -> 302 -> ConnectionRefused must not succeed");
    srv.join().unwrap();
    assert!(
        matches!(err1, api::site_maintenance::MaintenanceError::Unknown(_)),
        "expected MaintenanceError::Unknown on POST -> 302 -> ConnectionRefused, got {err1:?}"
    );
    assert_eq!(post_hits.load(Ordering::SeqCst), 1);
    let receipt = super::latest_checkin_receipt_from_state(&state, site_id);
    assert_eq!(receipt.state, SiteMaintenanceState::Unknown);

    let err2 = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_805,
        "http-checkin",
    )
    .err()
    .expect("retry in same window must be blocked");
    assert!(
        matches!(
            err2,
            api::site_maintenance::MaintenanceError::BlockedUnknown(_)
        ),
        "expected BlockedUnknown on retry, got {err2:?}"
    );
    assert_eq!(post_hits.load(Ordering::SeqCst), 1);
}

fn spawn_302_post_server(
    location: String,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_srv = std::sync::Arc::clone(&hits);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_srv = std::sync::Arc::clone(&stop);
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Acquire) {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut s, &mut buf);
                hits_srv.fetch_add(1, Ordering::SeqCst);
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = std::io::Write::write_all(&mut s, resp.as_bytes());
                let _ = std::io::Write::flush(&mut s);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    (addr, hits, stop, handle)
}

#[test]
fn wasm_maintenance_post_302_blocked_self_listen_freezes_unknown_without_duplicate_post() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let self_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let self_addr = self_listener.local_addr().unwrap();
    let self_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let self_hits_srv = std::sync::Arc::clone(&self_hits);
    let stop_self = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_self_srv = std::sync::Arc::clone(&stop_self);
    self_listener.set_nonblocking(true).unwrap();
    let self_srv = std::thread::spawn(move || {
        while !stop_self_srv.load(Ordering::Acquire) {
            if let Ok((mut s, _)) = self_listener.accept() {
                self_hits_srv.fetch_add(1, Ordering::SeqCst);
                let _ = std::io::Write::write_all(
                    &mut s,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
                );
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });

    let (site_addr, post_hits, stop_site, site_srv) =
        spawn_302_post_server(format!("http://{self_addr}/admin"));
    let store = std::sync::Arc::new(api::Store::open(dir.path().join("data")).unwrap());
    let dl = std::sync::Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let fetcher = std::sync::Arc::new(super::OfflineFetcherForTest);
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

    let site_id = SiteId::new();
    state
        .store()
        .insert_site(&Site {
            id: site_id,
            name: "WasmMaint302Self".into(),
            url: format!("http://{site_addr}"),
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
        })
        .unwrap();
    super::install_maint_plugin(&state, "org.example.maint-302-self", site_id);
    api::plugins::sync_registry_snapshot(&state, true).unwrap();

    let obs = state.store().observe_site(site_id).unwrap().unwrap();
    let provider_id = "plugin:org.example.maint-302-self/maint";
    let err1 = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_800,
        "http-checkin",
    )
    .err()
    .expect("POST -> 302 -> self_listen must not succeed");
    assert!(
        matches!(err1, api::site_maintenance::MaintenanceError::Unknown(_)),
        "expected MaintenanceError::Unknown on POST -> 302 -> self_listen, got {err1:?}"
    );
    assert_eq!(self_hits.load(Ordering::SeqCst), 0);
    assert_eq!(post_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        super::latest_checkin_receipt_from_state(&state, site_id).state,
        SiteMaintenanceState::Unknown
    );

    let err2 = api::site_maintenance::execute_receipt_maintenance(
        &state,
        &obs,
        indexer::MaintenanceAction::CheckIn,
        Some(provider_id),
        1_795_276_805,
        "http-checkin",
    )
    .err()
    .expect("retry in same window must be blocked");
    assert!(
        matches!(
            err2,
            api::site_maintenance::MaintenanceError::BlockedUnknown(_)
        ),
        "expected BlockedUnknown on retry, got {err2:?}"
    );
    assert_eq!(post_hits.load(Ordering::SeqCst), 1);

    stop_site.store(true, Ordering::Release);
    stop_self.store(true, Ordering::Release);
    site_srv.join().unwrap();
    self_srv.join().unwrap();
}

#[tokio::test]
async fn plugin_status_reports_credential_epoch_and_indexer_availability() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.statyaml";
    let yaml = "id: statdemo\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n";
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"Status YAML\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"Test\"]\ndescription = \"YAML\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\nsites = [\"sites/statdemo.yaml\"]\n"
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    let pkg = super::plugin_http_support::pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("sites/statdemo.yaml", yaml.as_bytes()),
    ]);
    let installed = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "en-stat")
        .await;

    let (st_c, _, b_c) = env
        .send(req_json(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            &[],
            Some(json!({
                "name": "StatusSite",
                "url": "https://status.example/",
                "profile_id": format!("plugin:{pid}/statdemo"),
                "cookie": "uid=1; pass=v1",
                "enabled": true,
            })),
        ))
        .await;
    let site_id = assert_ok(st_c, &b_c, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st1, _, b1) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/sites/{site_id}/plugin-status"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let s1 = assert_ok(st1, &b1, StatusCode::OK);
    let epoch1 = s1["credential_epoch"]
        .as_str()
        .expect("credential_epoch must be present")
        .to_string();
    assert_eq!(s1["available"], true);
    assert_eq!(s1["indexer_provider"]["availability"], "enabled");

    let (st_p, _, _) = env
        .send(req_json(
            "PATCH",
            &format!("/api/v1/sites/{site_id}"),
            Some(TOKEN),
            &[],
            Some(json!({ "cookie": "uid=1; pass=v2" })),
        ))
        .await;
    assert_eq!(st_p, StatusCode::OK);

    let (_, _, b_g) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let prev = b_g["data"]["revision"].as_i64().unwrap();
    env.disable_plugin(pid, prev, "dis-stat").await;

    let (st2, _, b2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/sites/{site_id}/plugin-status"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let s2 = assert_ok(st2, &b2, StatusCode::OK);
    let epoch2 = s2["credential_epoch"].as_str().unwrap();
    assert_ne!(epoch1, epoch2);
    assert_eq!(s2["available"], false);
    assert_eq!(s2["indexer_provider"]["availability"], "disabled");
}
