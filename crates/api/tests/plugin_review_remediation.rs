#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use api::plugins::hooks_runtime::{TransferRenameRequest, run_transfer_rename};
use axum::http::StatusCode;
use domain::{LibraryId, MediaId, MediaKind};
use plugin_api::{HOOK_WORLD, SiteCheckinAfterEvent};
use serde_json::{Value, json};
use store::plugin_events::{EmitOutboxEventInput, OutboxDb, OutboxDeliveryStatus};

use plugin_http_support::{HttpEnv, TOKEN, assert_ok, build_state, pkg, req_json};

const POLICY_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/policy.wasm");

struct WebhookStub {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    fail_mode: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl WebhookStub {
    fn start(initially_fail: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let fail_mode = Arc::new(AtomicBool::new(initially_fail));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_c = hits.clone();
        let fail_c = fail_mode.clone();
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let (code, body) = if fail_c.load(Ordering::SeqCst) {
                        (500, b"{\"ok\":false}".as_slice())
                    } else {
                        hits_c.fetch_add(1, Ordering::SeqCst);
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
            fail_mode,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for WebhookStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

async fn install_policy_plugin_with_grants(
    env: &HttpEnv,
    plugin_id: &str,
    hook_id: &str,
    manifest_perms: &[&str],
    grants: Value,
    webhook_addr: Option<SocketAddr>,
) {
    let perms_toml = manifest_perms
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let http_toml = webhook_addr
        .map(|addr| {
            format!(
                "\n[[http]]\norigin = \"http://{addr}\"\npath_prefix = \"/webhook\"\nmethods = [\"POST\"]\n"
            )
        })
        .unwrap_or_default();
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Policy {plugin_id}"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["test"]
description = "Review remediation test"
config_schema = "config.schema.json"
state_schema = 1
{http_toml}
[[components]]
id = "policy"
entry = "policy.wasm"
world = "{HOOK_WORLD}"
permissions = [{perms_toml}]

[[components.hooks]]
id = "{hook_id}"
priority = 10
"#
    );
    let pkg_bytes = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", POLICY_WASM),
    ]);
    let inst = env.install_pkg(&pkg_bytes, None, grants).await;
    let rev = inst["revision"].as_i64().unwrap();
    env.enable_plugin(plugin_id, rev, &format!("en-{plugin_id}"))
        .await;
}

#[tokio::test]
async fn p1_policy_hooks_enter_in_flight_record_metrics_and_honor_library_cancel() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let (lib_dir, lib_id) = create_movie_lib(&env, "movies", "Movies");

    install_policy_plugin_with_grants(
        &env,
        "org.example.p1rename",
        "transfer.rename",
        &["hooks:transfer.rename"],
        json!([{ "capability": "hooks:transfer.rename", "scope": "Global", "sensitive": false }]),
        None,
    )
    .await;

    let req = TransferRenameRequest {
        workflow_id: "wf-p1-1".into(),
        library_id: lib_id,
        library_root: lib_dir.clone(),
        media_id: MediaId::new(),
        media_kind: MediaKind::Movie,
        subscribe_id: None,
        source_path: PathBuf::from("/downloads/Film.mkv"),
        proposed_relative_path: PathBuf::from("Movie/Film.mkv"),
    };
    let renamed = run_transfer_rename(&env.state, &req);
    assert_eq!(renamed, lib_dir.join("renamed/Movie/Film.mkv"));

    let m1 = get_metrics(&env, "org.example.p1rename").await;
    assert_eq!(m1["total_invocations"], 1);
    assert_eq!(m1["total_errors"], 0);

    // Deleting the library cancels in-flight/future plugin invocations scoped to this library.
    let (st_del, _, _) = env
        .send(req_json(
            "DELETE",
            &format!("/api/v1/libraries/{lib_id}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(st_del, StatusCode::ACCEPTED);

    let after_cancel = run_transfer_rename(&env.state, &req);
    assert_eq!(
        after_cancel,
        lib_dir.join("Movie/Film.mkv"),
        "cancelled library must reject policy hook entry and fall back to default path"
    );

    let m2 = get_metrics(&env, "org.example.p1rename").await;
    assert_eq!(m2["total_invocations"], 1);
}

#[tokio::test]
async fn p1_notify_hook_honors_site_scoped_http_grant_and_records_error_metrics() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let site_id = env.insert_site("HDAreaSiteScoped");
    let srv = WebhookStub::start(true);

    install_policy_plugin_with_grants(
        &env,
        "org.example.p1sitehook",
        "site.checkin.after",
        &[
            "hooks:site.checkin.after",
            "events:site.checkin.after",
            "http",
        ],
        json!([
            { "capability": "hooks:site.checkin.after", "scope": "Global", "sensitive": false },
            { "capability": "events:site.checkin.after", "scope": "Global", "sensitive": false },
            { "capability": "http", "scope": { "Site": site_id.to_string() }, "sensitive": false },
        ]),
        Some(srv.addr),
    )
    .await;

    let payload = serde_json::to_string(&SiteCheckinAfterEvent {
        site_id: site_id.to_string(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-23".into(),
    })
    .unwrap();
    let t0 = store::unix_now_ms().unwrap();
    emit_checkin_event(&env, "ev-site-scoped-1", &site_id.to_string(), t0, payload);

    // First attempt hits webhook (site-scoped http grant authorized!) -> 500 schedules retry + records error metric.
    api::drain_plugin_events_for_test(&env.state, t0).await;
    let r1 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::App, "ev-site-scoped-1", "org.example.p1sitehook")
        .unwrap()
        .unwrap();
    assert_eq!(
        r1.status,
        OutboxDeliveryStatus::Pending,
        "site-scoped http grant should reach webhook and retry on 500 instead of dead-lettering: {:?}",
        r1.last_error
    );

    let m1 = get_metrics(&env, "org.example.p1sitehook").await;
    assert_eq!(m1["total_invocations"], 1);
    assert_eq!(m1["total_errors"], 1);

    srv.fail_mode.store(false, Ordering::SeqCst);
    api::drain_plugin_events_for_test(&env.state, t0 + 30_000).await;
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);

    let r2 = env
        .state
        .store()
        .get_delivery_receipt(OutboxDb::App, "ev-site-scoped-1", "org.example.p1sitehook")
        .unwrap()
        .unwrap();
    assert_eq!(r2.status, OutboxDeliveryStatus::Delivered);
}

#[tokio::test]
async fn p1_spawned_event_dispatcher_releases_store_arc_when_state_drops() {
    tokio::time::pause();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let state = build_state(dir.path());
    let weak_plugins = Arc::downgrade(state.plugins());
    let weak_store = Arc::downgrade(&state.store());

    // Verify explicit handle shutdown stops the task cleanly while state is alive.
    let handle = api::spawn_plugin_event_dispatcher_with_handle(state.clone());
    tokio::task::yield_now().await;
    handle.shutdown().await.unwrap();

    // Verify fire-and-forget spawn releases PluginService and Store once ApiState drops.
    api::spawn_plugin_event_dispatcher(state.clone());
    tokio::task::yield_now().await;
    state.probe.shutdown_workers().await.unwrap();
    state.chapter_frames().shutdown().await.unwrap();
    drop(state);

    for _ in 0..10 {
        if weak_plugins.upgrade().is_none() && weak_store.upgrade().is_none() {
            break;
        }
        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
    }
    assert!(
        weak_plugins.upgrade().is_none(),
        "background event dispatcher must not retain strong PluginService reference after ApiState drops"
    );
    assert!(
        weak_store.upgrade().is_none(),
        "background event dispatcher must not retain strong Store reference after ApiState drops"
    );
}

#[tokio::test]
async fn p2_event_dispatcher_prunes_delivered_outbox_rows_beyond_retention() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let srv = WebhookStub::start(false);

    install_policy_plugin_with_grants(
        &env,
        "org.example.p2prune",
        "site.checkin.after",
        &[
            "hooks:site.checkin.after",
            "events:site.checkin.after",
            "http",
        ],
        json!([
            { "capability": "hooks:site.checkin.after", "scope": "Global", "sensitive": false },
            { "capability": "events:site.checkin.after", "scope": "Global", "sensitive": false },
            { "capability": "http", "scope": "Global", "sensitive": false },
        ]),
        Some(srv.addr),
    )
    .await;

    let payload = serde_json::to_string(&SiteCheckinAfterEvent {
        site_id: "s-prune".into(),
        action: "check-in".into(),
        state: "completed".into(),
        window_key: "2026-11-23".into(),
    })
    .unwrap();

    let old_ms = 10_000_i64;
    let recent_ms = old_ms + 6 * 86_400_000;
    let prune_now_ms = old_ms + 8 * 86_400_000;
    for (ev_id, entity_id, ts) in [
        ("ev-prune-old", "s-prune-old", old_ms),
        ("ev-prune-recent", "s-prune-recent", recent_ms),
    ] {
        emit_checkin_event(&env, ev_id, entity_id, ts, payload.clone());
    }

    // Deliver both before retention window expires; both receipts should exist as Delivered.
    api::drain_plugin_events_for_test(&env.state, recent_ms).await;
    assert_eq!(srv.hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        receipt_status(&env, "ev-prune-old", "org.example.p2prune"),
        Some(OutboxDeliveryStatus::Delivered)
    );

    // Subsequent drain past the 7-day retention window prunes old delivered rows and keeps recent ones.
    api::drain_plugin_events_for_test(&env.state, prune_now_ms).await;

    assert!(
        receipt_status(&env, "ev-prune-old", "org.example.p2prune").is_none(),
        "delivered outbox event older than retention window must be pruned"
    );
    assert!(
        receipt_status(&env, "ev-prune-recent", "org.example.p2prune").is_some(),
        "delivered outbox event within retention window must be retained"
    );
}

#[tokio::test]
async fn p2_hooks_and_events_enumerate_plugins_beyond_first_100_page() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    // Seed 102 enabled padding plugins that sort lexicographically before `org.example.zlast`.
    for idx in 0..102 {
        let pid = format!("org.example.pad{idx:03}");
        let meta = store::plugins::PluginVersionMetadata {
            plugin_id: pid.clone(),
            package_hash: format!("{pid}-hash"),
            version: "1.0.0".into(),
            manifest_json: "{}".into(),
            config_schema_json: "{}".into(),
            state_schema: "1".into(),
            artifact_path: String::new(),
            signing_key_id: None,
            signature_status: "builtin".into(),
            ui_metadata_ref: None,
            credential_bound: false,
        };
        let rec = env.state.store().register_plugin_version(&meta, 0).unwrap();
        let op_id = uuid::Uuid::new_v4();
        let snap = env
            .state
            .store()
            .begin_plugin_enable_checked(&pid, rec.revision, op_id)
            .unwrap();
        env.state
            .store()
            .finalize_plugin_lifecycle_checked(
                &store::plugins::PluginLifecycleObservation {
                    plugin_id: pid,
                    revision: snap.plugin.revision,
                    generation: snap.plugin.generation,
                    operation_id: op_id,
                    workers_exited: true,
                    unknown_effects_reconciled: true,
                    validation_succeeded: true,
                },
                None,
            )
            .unwrap();
    }

    let (lib_dir, lib_id) = create_movie_lib(&env, "movies-p2", "MoviesP2");

    install_policy_plugin_with_grants(
        &env,
        "org.example.zlast",
        "transfer.rename",
        &["hooks:transfer.rename"],
        json!([{ "capability": "hooks:transfer.rename", "scope": "Global", "sensitive": false }]),
        None,
    )
    .await;

    let req = TransferRenameRequest {
        workflow_id: "wf-p2-1".into(),
        library_id: lib_id,
        library_root: lib_dir.clone(),
        media_id: MediaId::new(),
        media_kind: MediaKind::Movie,
        subscribe_id: None,
        source_path: PathBuf::from("/downloads/Film.mkv"),
        proposed_relative_path: PathBuf::from("Movie/Film.mkv"),
    };
    let renamed = run_transfer_rename(&env.state, &req);
    assert_eq!(
        renamed,
        lib_dir.join("renamed/Movie/Film.mkv"),
        "plugin past page 1 (>100 enabled plugins) must still execute policy hooks"
    );
}

fn receipt_status(env: &HttpEnv, ev_id: &str, pid: &str) -> Option<OutboxDeliveryStatus> {
    env.state
        .store()
        .get_delivery_receipt(OutboxDb::App, ev_id, pid)
        .unwrap()
        .map(|r| r.status)
}

fn create_movie_lib(env: &HttpEnv, dir_name: &str, name: &str) -> (PathBuf, LibraryId) {
    let lib_dir = env._dir.path().join(dir_name);
    std::fs::create_dir_all(&lib_dir).unwrap();
    let lib = env
        .state
        .store()
        .create_library(
            MediaKind::Movie,
            name,
            &[lib_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (lib_dir, lib.id.parse().unwrap())
}

async fn get_metrics(env: &HttpEnv, pid: &str) -> Value {
    let uri = format!("/api/v1/plugins/{pid}/metrics");
    let (st, _, body) = env
        .send(req_json("GET", &uri, Some(TOKEN), &[], None))
        .await;
    assert_ok(st, &body, StatusCode::OK)
}

fn emit_checkin_event(env: &HttpEnv, ev_id: &str, site_id: &str, ts: i64, payload_json: String) {
    env.state
        .store()
        .emit_outbox_event(
            OutboxDb::App,
            &EmitOutboxEventInput {
                event_id: ev_id.into(),
                schema_version: 1,
                event_domain: "site".into(),
                event_kind: "site.checkin.after".into(),
                entity_type: "site".into(),
                entity_id: site_id.into(),
                entity_sequence: None,
                entity_revision: 1,
                cause_id: format!("cause-{ev_id}"),
                subject: "system".into(),
                site_id: Some(site_id.into()),
                subscribe_id: None,
                library_id: None,
                handles_json: json!([site_id]).to_string(),
                payload_json,
                occurred_at: ts,
                created_at: ts,
            },
        )
        .unwrap();
}
