use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use plugin_api::{
    DownloadAfterEvent, EndpointRequest, ErrorCode, Failure, GuestRole, HOOK_WORLD, Header,
    SiteCheckinAfterEvent,
};
use plugin_host::Invocation;
use serde_json::json;
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};
use store::plugins::{PluginCommandCause, PluginSubject};

use crate::support::plugin_http_support::{HttpEnv, admin_id, pkg};

const IDX_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/indexer_fixture.wasm");
const MAINT_WASM: &[u8] =
    include_bytes!("../../../plugin-host/tests/fixtures/maintenance_fixture.wasm");
const EP_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/endpoint_fixture.wasm");
const POLICY_WASM: &[u8] = include_bytes!("../../../plugin-host/tests/fixtures/policy.wasm");

fn build_provider_collision_zip(pid: &str) -> Vec<u8> {
    let cfg_schema = pkg::sample_schema(false, None);
    let site_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R6 Provider Sub-Declaration Isolation"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Sub-declaration precedence over component id"
config_schema = "schemas/config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "beta"
entry = "components/idx_bare.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide"]

[[components.indexers]]
id = "fixture"
name = "Bare Indexer"
kind = "torznab"
capabilities = ["search", "rss", "detail", "resolve-download", "user-stats", "categories"]
site_schema = "schemas/site.schema.json"

[[components]]
id = "maint-x"
entry = "components/m_priv.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["maintenance:provide", "kv"]

[[components.maintenance]]
id = "m-priv"
actions = ["check-in"]

[[components]]
id = "maint-y"
entry = "components/m_bare.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["maintenance:provide"]

[[components.maintenance]]
id = "maint-x"
actions = ["check-in"]
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/site.schema.json", site_schema.as_bytes()),
        ("components/idx_bare.wasm", IDX_WASM),
        ("components/m_priv.wasm", MAINT_WASM),
        ("components/m_bare.wasm", MAINT_WASM),
    ])
}

#[tokio::test]
async fn slice_d_r6_item2a_provider_subdeclaration_precedes_comp_id_and_fails_closed_on_ambiguous()
{
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("R6-ProvSite");
    let store = env.state.store();

    let pid = "org.example.r6-prov-iso";
    let zip = build_provider_collision_zip(pid);
    let grants = json!([
        { "capability": "indexer:provide", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "maintenance:provide", "scope": { "Site": site_id }, "sensitive": false },
        { "capability": "kv", "scope": { "Site": site_id }, "sensitive": false }
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r6-pi-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let epoch = store
        .observe_site(site_id)
        .unwrap()
        .unwrap()
        .credential_epoch()
        .storage_key()
        .to_string();
    let ports = env.state.plugins().host_ports();

    // 1. Maintenance sub-declaration `maint-x` on `maint-y` (permissions = ["maintenance:provide"])
    // must take precedence over component `id = "maint-x"` (permissions = ["maintenance:provide", "kv"]).
    for raw_pid in ["maint-x".to_string(), format!("plugin:{pid}/maint-x")] {
        let mut inv_m = Invocation::new(pid, Duration::from_secs(5));
        inv_m.generation = u64::try_from(enabled.generation).unwrap();
        inv_m.role = GuestRole::Maintenance;
        inv_m.provider_id = Some(raw_pid);
        inv_m.site_id = Some(site_id.to_string());
        inv_m.credential_epoch = Some(epoch.clone());
        let g_m = ports.begin_invocation(&inv_m).unwrap();
        assert!(
            !ports.authorized(&inv_m, "kv"),
            "sub-decl `maint-x` on `maint-y` must not inherit `maint-x` comp `kv`"
        );
        drop(g_m);
    }

    // Fallback to `c.id == "maint-y"` when zero `m.id` match `"maint-y"`.
    let mut inv_fb = Invocation::new(pid, Duration::from_secs(5));
    inv_fb.generation = u64::try_from(enabled.generation).unwrap();
    inv_fb.role = GuestRole::Maintenance;
    inv_fb.provider_id = Some("maint-y".into());
    inv_fb.site_id = Some(site_id.to_string());
    inv_fb.credential_epoch = Some(epoch.clone());
    let g_fb = ports.begin_invocation(&inv_fb).unwrap();
    assert!(!ports.authorized(&inv_fb, "kv"));
    drop(g_fb);

    assert_indexer_subdecl_precedence_and_ambiguity(&env, pid, enabled.generation, site_id, &epoch);
}

fn assert_indexer_subdecl_precedence_and_ambiguity(
    env: &HttpEnv,
    pid: &str,
    generation: i64,
    site_id: domain::SiteId,
    epoch: &str,
) {
    let store = env.state.store();
    let ver = store
        .get_plugin_version(pid, &store.get_plugin(pid).unwrap().unwrap().package_hash)
        .unwrap()
        .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&ver.manifest_json).unwrap();
    // Prepend an INDEXER_WORLD component whose `c.id == "fixture"` has `kv`, while `beta` owns `idx.id == "fixture"` without `kv`.
    let mut comps = manifest["components"].as_array().unwrap().clone();
    let mut comp_id_collision = comps[0].clone();
    comp_id_collision["id"] = json!("fixture");
    comp_id_collision["permissions"] = json!(["indexer:provide", "kv"]);
    comp_id_collision["indexers"][0]["id"] = json!("other-sub");
    comps.insert(0, comp_id_collision);
    manifest["components"] = json!(comps);
    let updated_json = serde_json::to_string(&manifest).unwrap();
    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE plugin_versions SET manifest_json = ?1 WHERE plugin_id = ?2",
                rusqlite::params![updated_json, pid],
            )?;
            Ok(())
        })
        .unwrap();

    let ports = env.state.plugins().host_ports();
    let mut inv_idx = Invocation::new(pid, Duration::from_secs(5));
    inv_idx.generation = u64::try_from(generation).unwrap();
    inv_idx.role = GuestRole::Indexer;
    inv_idx.provider_id = Some("fixture".into());
    inv_idx.site_id = Some(site_id.to_string());
    inv_idx.credential_epoch = Some(epoch.into());
    let g_idx = ports.begin_invocation(&inv_idx).unwrap();
    assert!(
        !ports.authorized(&inv_idx, "kv"),
        "indexer sub-decl `fixture` on `beta` must not inherit `c.id == fixture` comp `kv`"
    );
    drop(g_idx);

    // Now make two INDEXER_WORLD components declare `idx.id == "fixture"` -> must fail closed!
    manifest["components"][0]["indexers"][0]["id"] = json!("fixture");
    let dup_json = serde_json::to_string(&manifest).unwrap();
    store
        .with_writer(store::Db::App, |c| {
            c.execute(
                "UPDATE plugin_versions SET manifest_json = ?1 WHERE plugin_id = ?2",
                rusqlite::params![dup_json, pid],
            )?;
            Ok(())
        })
        .unwrap();

    let mut inv_dup = Invocation::new(pid, Duration::from_secs(5));
    inv_dup.generation = u64::try_from(generation).unwrap();
    inv_dup.role = GuestRole::Indexer;
    inv_dup.provider_id = Some("fixture".into());
    inv_dup.site_id = Some(site_id.to_string());
    inv_dup.credential_epoch = Some(epoch.into());
    assert_eq!(
        ports.begin_invocation(&inv_dup).err().unwrap().code,
        ErrorCode::PermissionDenied
    );
}

fn build_ep_collision_zip(pid: &str) -> Vec<u8> {
    let cfg_schema = pkg::sample_schema(false, None);
    let ep_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#;
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R6 Endpoint Precedence"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Endpoint path precedence and ambiguity fail-closed"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "ep-id-collision"
entry = "components/ep_priv.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["kv"]

[[components.endpoints]]
id = "kv-put"
method = "POST"
path = "/admin/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components.endpoints]]
id = "dup-route-a"
method = "POST"
path = "/dup-route"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components]]
id = "ep-exact-path"
entry = "components/ep_bare.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = []

[[components.endpoints]]
id = "other-id"
method = "POST"
path = "/kv-put"
auth = "admin"
input_schema = "schemas/ep.schema.json"

[[components.endpoints]]
id = "dup-route-b"
method = "POST"
path = "/dup-route"
auth = "admin"
input_schema = "schemas/ep.schema.json"
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/ep.schema.json", ep_schema.as_bytes()),
        ("components/ep_priv.wasm", EP_WASM),
        ("components/ep_bare.wasm", EP_WASM),
    ])
}

#[tokio::test]
async fn slice_d_r6_item2b_endpoint_path_matches_before_id_fallback_and_rejects_duplicates() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r6-ep-iso";
    let zip = build_ep_collision_zip(pid);
    let grants = json!([{ "capability": "kv", "scope": "Global", "sensitive": false }]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r6-ep-en")
        .await;

    let enabled = store.get_plugin(pid).unwrap().unwrap();
    let auth = store.issue_plugin_state_authority(&enabled, None).unwrap();
    let subject = PluginSubject::User(admin_id());

    // Exact `ep.path == "/kv-put"` must pick `ep-exact-path` (permissions = []), NOT `ep-id-collision` (`ep.id == "kv-put"`, permissions = ["kv"]).
    let exact_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth.clone(),
            subject.clone(),
            PluginCommandCause::root("r6-ep-exact"),
            EndpointRequest {
                method: "POST".into(),
                path: "/kv-put".into(),
                query: Vec::<Header>::new(),
                body: b"must-be-denied".to_vec(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(exact_err, Failure::Guest(ref e) if e.code == ErrorCode::PermissionDenied),
        "expected PermissionDenied from ep-exact-path, got {exact_err:?}"
    );

    // Duplicate `POST /dup-route` across two components must fail closed with Conflict / InvalidComponent.
    let dup_err = env
        .state
        .plugins()
        .invoke_endpoint(
            &env.state,
            &enabled,
            auth,
            subject,
            PluginCommandCause::root("r6-ep-dup"),
            EndpointRequest {
                method: "POST".into(),
                path: "/dup-route".into(),
                query: Vec::<Header>::new(),
                body: Vec::new(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(dup_err, Failure::Guest(ref e) if e.code == ErrorCode::Conflict)
            || matches!(dup_err, Failure::InvalidComponent(_)),
        "expected Conflict or InvalidComponent for duplicate endpoint route, got {dup_err:?}"
    );
}

struct SequencedWebhookStub {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    fail_on_hit: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SequencedWebhookStub {
    fn start(fail_on_hit: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let fail_at = Arc::new(AtomicUsize::new(fail_on_hit));
        let stop = Arc::new(AtomicBool::new(false));
        let (hits_c, fail_c, stop_c) = (hits.clone(), fail_at.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf);
                    let hit_num = hits_c.fetch_add(1, Ordering::SeqCst) + 1;
                    let (code, body) = if hit_num == fail_c.load(Ordering::SeqCst) {
                        (500, b"{\"ok\":false}".as_slice())
                    } else {
                        (200, b"{\"ok\":true}".as_slice())
                    };
                    let head = format!(
                        "HTTP/1.1 {code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            addr,
            hits,
            fail_on_hit: fail_at,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for SequencedWebhookStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[tokio::test]
async fn slice_d_r6_item2c_outbox_delivers_all_sorted_hook_components_and_retries_on_partial_failure()
 {
    let stub = SequencedWebhookStub::start(0);
    let mut env = HttpEnv::new().await;
    env.state = env.state.with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(stub.addr),
    );
    env.app = api::router(env.state.clone());
    env.enable_dev_mode().await;

    let pid = "org.example.r6-multi-evt";
    let zip = build_multi_hook_event_zip(pid, stub.addr);
    let grants = json!([
        { "capability": "hooks:download.after", "scope": "Global", "sensitive": false },
        { "capability": "events:download.after", "scope": "Global", "sensitive": false },
        { "capability": "hooks:site.checkin.after", "scope": "Global", "sensitive": false },
        { "capability": "events:site.checkin.after", "scope": "Global", "sensitive": false },
        { "capability": "http", "scope": "Global", "sensitive": false }
    ]);
    let inst = env.install_pkg(&zip, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r6-me-en")
        .await;

    let t0 = store::unix_now_ms().unwrap();
    emit_download_event(&env, "ev-r6-all-ok", t0);
    emit_checkin_event(&env, "ev-r6-checkin-order", t0);
    api::drain_plugin_events_for_test(&env.state, t0).await;

    assert_eq!(
        stub.hits.load(Ordering::SeqCst),
        6,
        "2 events * 3 HOOK_WORLD components = 6 notifications"
    );
    let r1 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r6-all-ok", pid)
        .unwrap()
        .unwrap();
    assert_eq!(r1.status, OutboxDeliveryStatus::Delivered);
    let traces = env.state.plugins().take_recent_checkin_traces_for_test();
    assert_eq!(traces.len(), 1);
    let comps: Vec<&str> = traces[0]
        .steps
        .iter()
        .map(|s| s.component.as_str())
        .collect();
    assert_eq!(comps, vec!["comp-a", "comp-b", "comp-c"]);

    // Fail on the 2nd component of the 2nd download event -> healthy siblings still run once,
    // delivery schedules retry, and retry only re-runs the failed component!
    stub.hits.store(0, Ordering::SeqCst);
    stub.fail_on_hit.store(2, Ordering::SeqCst);
    emit_download_event(&env, "ev-r6-partial-fail", t0 + 100);
    api::drain_plugin_events_for_test(&env.state, t0 + 100).await;
    assert_eq!(
        stub.hits.load(Ordering::SeqCst),
        3,
        "all 3 components run on Attempt 1 without starving comp-c"
    );
    let r2_pending = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r6-partial-fail", pid)
        .unwrap()
        .unwrap();
    assert_ne!(r2_pending.status, OutboxDeliveryStatus::Delivered);

    stub.fail_on_hit.store(0, Ordering::SeqCst);
    api::drain_plugin_events_for_test(&env.state, t0 + 5_000).await;
    let r2_done = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::Subscribe, "ev-r6-partial-fail", pid)
        .unwrap()
        .unwrap();
    assert_eq!(r2_done.status, OutboxDeliveryStatus::Delivered);
    assert_eq!(
        stub.hits.load(Ordering::SeqCst),
        4,
        "retry must only re-run failed comp-b without duplicating comp-a or comp-c"
    );
}

fn build_multi_hook_event_zip(pid: &str, addr: SocketAddr) -> Vec<u8> {
    let schema = pkg::sample_schema(false, None);
    let perms = r#"["hooks:download.after", "events:download.after", "hooks:site.checkin.after", "events:site.checkin.after", "http"]"#;
    let manifest = format!(
        r#"manifest_version = 1
id = "{pid}"
name = "R6 Multi Event"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Multi HOOK_WORLD outbox delivery"
config_schema = "config.schema.json"
state_schema = 1

[[http]]
origin = "http://{addr}"
path_prefix = "/webhook"
methods = ["POST"]

[[components]]
id = "comp-c"
entry = "c.wasm"
world = "{HOOK_WORLD}"
permissions = {perms}
[[components.hooks]]
id = "download.after"
priority = 20
[[components.hooks]]
id = "site.checkin.after"
priority = 20

[[components]]
id = "comp-b"
entry = "b.wasm"
world = "{HOOK_WORLD}"
permissions = {perms}
[[components.hooks]]
id = "download.after"
priority = 10
[[components.hooks]]
id = "site.checkin.after"
priority = 10

[[components]]
id = "comp-a"
entry = "a.wasm"
world = "{HOOK_WORLD}"
permissions = {perms}
[[components.hooks]]
id = "download.after"
priority = 10
[[components.hooks]]
id = "site.checkin.after"
priority = 10
"#
    );
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("a.wasm", POLICY_WASM),
        ("b.wasm", POLICY_WASM),
        ("c.wasm", POLICY_WASM),
    ])
}

fn emit_download_event(env: &HttpEnv, event_id: &str, now_ms: i64) {
    let payload = serde_json::to_string(&DownloadAfterEvent {
        operation_id: format!("op-{event_id}"),
        downloader_id: Some("dl-1".into()),
        info_hash: Some("hash-1".into()),
        outcome: "submitted".into(),
        verified_total_bytes: Some(1024),
    })
    .unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::Subscribe,
            &EmitOutboxEventInput {
                event_id: event_id.into(),
                schema_version: 1,
                event_domain: "download".into(),
                event_kind: "download.after".into(),
                entity_type: "download_operation".into(),
                entity_id: format!("op-{event_id}"),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{event_id}"),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[\"h1\"]".into(),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}

fn emit_checkin_event(env: &HttpEnv, event_id: &str, now_ms: i64) {
    let payload = serde_json::to_string(&SiteCheckinAfterEvent {
        site_id: domain::SiteId::new().to_string(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-25".into(),
    })
    .unwrap();
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: event_id.into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: format!("ent-{event_id}"),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{event_id}"),
                subject: "system".into(),
                site_id: None,
                subscribe_id: None,
                library_id: None,
                handles_json: "[]".into(),
                payload_json: payload,
                occurred_at: now_ms,
                created_at: now_ms,
            },
        )
        .unwrap();
}
