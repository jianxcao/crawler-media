#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use api::plugins::recover_startup;
use api::{ApiState, Store};
use axum::http::StatusCode;
use domain::{MaintenanceVia, Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, MaintenanceAction, RegistrySnapshot};
use plugin_http_support::{HttpEnv, TOKEN, assert_ok, pkg::build_stored_zip, req_json};
use serde_json::json;

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");
const MAINTENANCE_WASM: &[u8] =
    include_bytes!("../../plugin-host/tests/fixtures/maintenance_fixture.wasm");
const TASK_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/task_fixture.wasm");

struct OfflineFetcher;
impl Fetcher for OfflineFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn build_indexer_maint_package(
    plugin_id: &str,
    yaml_id: &str,
    indexer_id: &str,
    maint_id: &str,
    include_yaml: bool,
) -> Vec<u8> {
    let mut entries: Vec<(&str, Vec<u8>)> = Vec::new();
    let sites_arr = if include_yaml {
        let yaml = format!(
            "id: {yaml_id}\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n"
        );
        entries.push(("sites/custom.yaml", yaml.into_bytes()));
        "sites = [\"sites/custom.yaml\"]\n"
    } else {
        ""
    };
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Registry Lifecycle Fixture"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Lifecycle test plugin"
config_schema = "config.schema.json"
state_schema = 1
credential_binding = "invocation-site"
{sites_arr}
[[components]]
id = "idx-comp"
entry = "indexer.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["http", "http:site-auth", "read:sites", "indexer:provide"]

[[components.indexers]]
id = "{indexer_id}"
name = "Fixture Indexer"
kind = "pt"
capabilities = ["search", "rss", "detail", "resolve-download", "user-stats", "categories"]
site_schema = "site.schema.json"

[[components]]
id = "maint-comp"
entry = "maint.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["http", "http:site-auth", "read:sites", "maintenance:provide"]

[[components.maintenance]]
id = "{maint_id}"
actions = ["verify-session", "check-in"]
"#
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    entries.push(("plugin.toml", manifest.into_bytes()));
    entries.push(("config.schema.json", schema.as_bytes().to_vec()));
    entries.push(("site.schema.json", schema.as_bytes().to_vec()));
    entries.push(("indexer.wasm", INDEXER_WASM.to_vec()));
    entries.push(("maint.wasm", MAINTENANCE_WASM.to_vec()));
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    build_stored_zip(&refs)
}

fn spawn_snapshot_stub() -> std::net::SocketAddr {
    let srv = TcpListener::bind("127.0.0.1:0").unwrap();
    let srv_addr = srv.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = srv.accept() {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let mut total = 0;
            while total < buf.len() {
                match stream.read(&mut buf[total..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        total += n;
                        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let body = b"live-from-snapshot";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    srv_addr
}

fn insert_reg_site(store: &Store, srv_addr: std::net::SocketAddr) -> Site {
    let site_id = SiteId::new();
    let site = Site {
        id: site_id,
        name: "SnapSite".into(),
        url: format!("http://{srv_addr}"),
        profile_id: "plugin:org.example.reg/fixture".into(),
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
    store.insert_site(&site).unwrap();
    site
}

#[tokio::test]
async fn plugin_enable_disable_revoke_uninstall_updates_registry_snapshot() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;

    let srv_addr = spawn_snapshot_stub();
    let site = insert_reg_site(env.state.store().as_ref(), srv_addr);
    let site_id = site.id;

    let pkg = build_indexer_maint_package("org.example.reg", "yaml-site", "fixture", "maint", true);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "indexer:provide", "scope": "Global", "sensitive": false},
        {"capability": "maintenance:provide", "scope": "Global", "sensitive": false}
    ]);
    let installed = env.install_pkg(&pkg, None, grants).await;
    let gen0 = env.state.indexer().snapshot().generation();

    let enabled_op = env
        .enable_plugin(
            "org.example.reg",
            installed["revision"].as_i64().unwrap(),
            "en-reg-1",
        )
        .await;
    assert_eq!(enabled_op["state"], "succeeded");

    let snap1 = env.state.indexer().snapshot();
    assert!(snap1.generation() > gen0);
    assert!(snap1.profile("plugin:org.example.reg/yaml-site").is_some());
    assert!(snap1.indexer("plugin:org.example.reg/fixture").is_some());
    let maint = snap1
        .maintenance("plugin:org.example.reg/maint")
        .expect("maintenance provider registered");
    assert!(
        maint
            .descriptor()
            .actions
            .contains(&MaintenanceAction::CheckIn)
    );

    let outcome = env.state.indexer().search(&[site], "site-auth-http");
    assert!(
        outcome.failures.is_empty(),
        "failures: {:?}",
        outcome.failures
    );
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(outcome.torrents[0].title, "auth:live-from-snapshot");

    let rev = env.get_plugin_rev("org.example.reg").await;
    env.disable_plugin("org.example.reg", rev, "dis-reg-1")
        .await;
    let snap2 = env.state.indexer().snapshot();
    assert!(snap2.generation() > snap1.generation());
    assert!(snap2.profile("plugin:org.example.reg/yaml-site").is_none());
    assert!(snap2.indexer("plugin:org.example.reg/fixture").is_none());
    assert!(snap2.maintenance("plugin:org.example.reg/maint").is_none());

    let rev2 = env.get_plugin_rev("org.example.reg").await;
    env.enable_plugin("org.example.reg", rev2, "en-reg-2").await;
    assert!(
        env.state
            .indexer()
            .snapshot()
            .indexer("plugin:org.example.reg/fixture")
            .is_some()
    );

    let rev3 = env.get_plugin_rev("org.example.reg").await;
    let if_match3 = format!("\"{rev3}\"");
    let (st_r, _, b_r) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.reg/permissions/revoke",
            Some(TOKEN),
            &[("idempotency-key", "rev-reg-1"), ("if-match", &if_match3)],
            Some(json!({
                "reason": "test revoke",
                "grants": [{"capability": "http", "scope": "Global", "sensitive": false}]
            })),
        ))
        .await;
    assert_ok(st_r, &b_r, StatusCode::ACCEPTED);
    let snap3 = env.state.indexer().snapshot();
    assert!(snap3.indexer("plugin:org.example.reg/fixture").is_none());
    assert!(snap3.maintenance("plugin:org.example.reg/maint").is_none());
}

#[tokio::test]
async fn colliding_provider_candidate_is_rejected_without_mutating_snapshot() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg =
        build_indexer_maint_package("org.example.collide", "fixture", "fixture", "maint", true);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "indexer:provide", "scope": "Global", "sensitive": false},
        {"capability": "maintenance:provide", "scope": "Global", "sensitive": false}
    ]);
    let installed = env.install_pkg(&pkg, None, grants).await;
    let before = env.state.indexer().snapshot();

    let if_match = format!("\"{}\"", installed["revision"].as_i64().unwrap());
    let (st_e, _, b_e) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/org.example.collide/enable",
            Some(TOKEN),
            &[("idempotency-key", "en-collide"), ("if-match", &if_match)],
            Some(json!({})),
        ))
        .await;
    let op = assert_ok(st_e, &b_e, StatusCode::ACCEPTED);
    assert_eq!(op["state"], "failed");

    let after = env.state.indexer().snapshot();
    assert_eq!(after.generation(), before.generation());
    assert!(
        after
            .indexer("plugin:org.example.collide/fixture")
            .is_none()
    );
}

#[tokio::test]
async fn startup_recovery_restores_enabled_plugin_providers_and_overlay_profiles() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg =
        build_indexer_maint_package("org.example.recov", "yaml-rec", "fixture", "maint", true);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "indexer:provide", "scope": "Global", "sensitive": false},
        {"capability": "maintenance:provide", "scope": "Global", "sensitive": false}
    ]);
    let installed = env.install_pkg(&pkg, None, grants).await;
    let op = env
        .enable_plugin(
            "org.example.recov",
            installed["revision"].as_i64().unwrap(),
            "en-recov",
        )
        .await;
    assert_eq!(op["state"], "succeeded");

    let data_dir = env.state.store().data_dir().to_path_buf();
    let overlay_dir = env._dir.path().join("overlay");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    std::fs::write(
        overlay_dir.join("overlay-site.yaml"),
        "id: overlay-site\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n",
    )
    .unwrap();
    drop(env.app);
    drop(env.state);

    let reopened_store = Arc::new(Store::open(&data_dir).unwrap());
    let dl = Arc::new(MemoryDownloader::new(data_dir.join("stage2")));
    let reopened = ApiState::new_arc(
        reopened_store,
        TOKEN.into(),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(OfflineFetcher),
        dl,
    )
    .unwrap()
    .with_indexer_overlay_dir(&overlay_dir);

    recover_startup(&reopened).unwrap();
    let snap = reopened.indexer().snapshot();
    assert!(snap.profile("overlay-site").is_some());
    assert!(snap.profile("plugin:org.example.recov/yaml-rec").is_some());
    assert!(snap.indexer("plugin:org.example.recov/fixture").is_some());
    assert!(snap.maintenance("plugin:org.example.recov/maint").is_some());
}

fn build_non_provider_task_package(plugin_id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Non-Provider Task"
version = "{version}"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Task only"
config_schema = "config.schema.json"
state_schema = 1

[[components]]
id = "task-comp"
entry = "task.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv"]

[[components.tasks]]
id = "sync"
input_schema = "task.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 60
"#
    );
    let cfg_schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", cfg_schema.as_bytes()),
        ("task.schema.json", cfg_schema.as_bytes()),
        ("task.wasm", TASK_WASM),
    ])
}

fn build_maintenance_only_package(plugin_id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{plugin_id}"
name = "Maintenance Only"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test"]
description = "Maintenance only"
config_schema = "config.schema.json"
state_schema = 1
credential_binding = "invocation-site"

[[components]]
id = "maint-comp"
entry = "maint.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["http", "http:site-auth", "read:sites", "maintenance:provide"]

[[components.maintenance]]
id = "maint"
actions = ["verify-session", "check-in"]
"#
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("maint.wasm", MAINTENANCE_WASM),
    ])
}

async fn prepare_and_switch_pkg(
    env: &HttpEnv,
    pid: &str,
    rev: i64,
    version: &str,
    upload_id: Option<&str>,
    package_hash: Option<&str>,
    rollback: bool,
) {
    let (_, _, b_sc) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins/{pid}/config-schema"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let srev = b_sc["data"]["schema_revision"].as_str().unwrap_or("1");
    let mut prep_req = json!({
        "version": version,
        "target_config": { "schema_revision": srev, "values": {} },
        "state_policy": "reset"
    });
    if let Some(uid) = upload_id {
        prep_req["upload_id"] = json!(uid);
    }
    if let Some(h) = package_hash {
        prep_req["package_hash"] = json!(h);
    }
    let if_match = format!("\"{rev}\"");
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/upgrades/prepare"),
            Some(TOKEN),
            &[
                ("if-match", &if_match),
                (
                    "idempotency-key",
                    &format!("prep-{pid}-{version}-{rollback}"),
                ),
            ],
            Some(prep_req),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::ACCEPTED)["result"].clone();
    let reauth = env
        .reauth(if rollback {
            "plugin:rollback"
        } else {
            "plugin:upgrade"
        })
        .await;
    let endpoint = if rollback { "rollback" } else { "upgrade" };
    let mut exec_req = json!({
        "preparation_id": prep["preparation_id"],
        "consent_token": prep["consent_token"],
        "reauth_token": reauth,
        "grants": [],
        "state_policy": "reset",
        "target_config_digest": prep["config_diff"]["target_config_digest"]
    });
    if let Some(h) = package_hash {
        exec_req["package_hash"] = json!(h);
    }
    let (st_u, _, b_u) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/{endpoint}"),
            Some(TOKEN),
            &[
                ("if-match", &if_match),
                (
                    "idempotency-key",
                    &format!("exec-{pid}-{version}-{rollback}"),
                ),
            ],
            Some(exec_req),
        ))
        .await;
    assert_eq!(
        assert_ok(st_u, &b_u, StatusCode::ACCEPTED)["state"],
        "succeeded"
    );
}

#[tokio::test]
async fn startup_with_overlay_dir_publishes_overlay_and_builtin_maintenance_provider() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let overlay_dir = tmp.path().join("overlay");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    std::fs::write(
        overlay_dir.join("overlay-startup.yaml"),
        "id: overlay-startup\nframework: nexusphp\nrender: false\nsearch:\n  path: /torrents.php\n  query_param: search\nlist:\n  item: tr.torrent\nfields:\n  title:\n    selector: a.name\n  enclosure:\n    selector: a.download\n    attr: href\n  detail:\n    selector: a.name\n    attr: href\n  size:\n    selector: td.size\n  seeders:\n    selector: td.seeders\n  leechers:\n    selector: td.leechers\n  snatched:\n    selector: td.snatched\n  upload_time:\n    selector: td.time\n  category:\n    selector: td.cat\n  free:\n    selector: img.free\n  hr:\n    selector: img.hr\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    let dl = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = ApiState::new_arc(
        store,
        TOKEN.into(),
        RegistrySnapshot::load_with_overlay(Some(&overlay_dir)).unwrap(),
        Arc::new(OfflineFetcher),
        dl,
    )
    .unwrap()
    .with_indexer_overlay_dir(&overlay_dir);

    let snap = state.indexer().snapshot();
    assert!(snap.profile("overlay-startup").is_some());
    assert!(
        snap.maintenance("plugin:builtin.nexusphp-maintenance/default")
            .is_some(),
        "builtin maintenance provider must be published even when overlay profiles exist"
    );

    let tmp2 = tempfile::tempdir_in("/tmp").unwrap();
    let store2 = Arc::new(Store::open(tmp2.path().join("data")).unwrap());
    let dl2 = Arc::new(MemoryDownloader::new(tmp2.path().join("dl")));
    let state2 = ApiState::new_arc_with_bootstrap_options(
        store2,
        RegistrySnapshot::load_with_overlay(Some(&overlay_dir)).unwrap(),
        Arc::new(OfflineFetcher),
        dl2,
        api::bootstrap_credentials::AdminBootstrapAction::Seed {
            password: "secret-password".into(),
            cli_token: TOKEN.into(),
        },
        api::ApiStateBootstrapOptions {
            indexer_overlay_dir: Some(overlay_dir),
            ..Default::default()
        },
    )
    .unwrap();
    let snap2 = state2.indexer().snapshot();
    assert!(snap2.profile("overlay-startup").is_some());
    assert!(
        snap2
            .maintenance("plugin:builtin.nexusphp-maintenance/default")
            .is_some()
    );
}

#[tokio::test]
async fn upgrade_to_non_provider_and_rollback_remove_stale_providers_from_snapshot() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.upg-stale";
    let pkg1 = build_indexer_maint_package(pid, "yaml-u1", "fixture", "maint", true);
    let grants1 = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "indexer:provide", "scope": "Global", "sensitive": false},
        {"capability": "maintenance:provide", "scope": "Global", "sensitive": false}
    ]);
    let inst = env.install_pkg(&pkg1, None, grants1).await;
    let hash1 = inst["package_hash"].as_str().unwrap().to_string();
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-u1")
        .await;
    let snap1 = env.state.indexer().snapshot();
    assert!(
        snap1
            .indexer("plugin:org.example.upg-stale/fixture")
            .is_some()
    );
    assert!(
        snap1
            .maintenance("plugin:org.example.upg-stale/maint")
            .is_some()
    );

    let pkg2 = build_non_provider_task_package(pid, "2.0.0");
    let up2 = env.upload_pkg(&pkg2, None, "up-u2").await;
    let rev1 = env.get_plugin_rev(pid).await;
    prepare_and_switch_pkg(
        &env,
        pid,
        rev1,
        "2.0.0",
        up2["upload_id"].as_str(),
        None,
        false,
    )
    .await;

    let snap2 = env.state.indexer().snapshot();
    assert!(
        snap2
            .profile("plugin:org.example.upg-stale/yaml-u1")
            .is_none()
    );
    assert!(
        snap2
            .indexer("plugin:org.example.upg-stale/fixture")
            .is_none()
    );
    assert!(
        snap2
            .maintenance("plugin:org.example.upg-stale/maint")
            .is_none()
    );

    let rev2 = env.get_plugin_rev(pid).await;
    prepare_and_switch_pkg(&env, pid, rev2, "1.0.0", None, Some(&hash1), true).await;
    let snap3 = env.state.indexer().snapshot();
    assert!(
        snap3
            .indexer("plugin:org.example.upg-stale/fixture")
            .is_none()
    );
    assert!(
        snap3
            .maintenance("plugin:org.example.upg-stale/maint")
            .is_none()
    );
}

#[tokio::test]
async fn failed_switch_after_drain_and_maintenance_only_sync_evict_providers() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.maintonly";
    let pkg = build_maintenance_only_package(pid);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "maintenance:provide", "scope": "Global", "sensitive": false}
    ]);
    let inst = env.install_pkg(&pkg, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-mo")
        .await;
    assert!(
        env.state
            .indexer()
            .snapshot()
            .maintenance("plugin:org.example.maintonly/maint")
            .is_some()
    );

    // Also disable builtin.nexusphp-maintenance so no provider plugins remain enabled in DB,
    // verifying force=false sync evicts stale maintenance-only DB providers too.
    let builtin_rev = env.get_plugin_rev("builtin.nexusphp-maintenance").await;
    env.disable_plugin("builtin.nexusphp-maintenance", builtin_rev, "dis-bi")
        .await;
    // Now disable maintonly in DB without syncing, then call force=false sync.
    let p = env.state.store().get_plugin(pid).unwrap().unwrap();
    let snap_d = env
        .state
        .store()
        .begin_plugin_lifecycle(pid, p.revision, uuid::Uuid::new_v4(), false)
        .unwrap();
    env.state
        .store()
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: snap_d.plugin_id,
            revision: snap_d.revision,
            generation: snap_d.generation,
            operation_id: snap_d.active_operation_id.unwrap(),
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    api::plugins::sync_registry_snapshot(&env.state, false).unwrap();
    assert!(
        env.state
            .indexer()
            .snapshot()
            .maintenance("plugin:org.example.maintonly/maint")
            .is_none(),
        "force=false sync must evict stale maintenance-only DB plugin"
    );
}

#[tokio::test]
async fn ungranted_indexer_and_maintenance_plugins_are_not_registered_in_snapshot() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pid = "org.example.nogrant";
    let pkg = build_indexer_maint_package(pid, "yaml-ng", "fixture", "maint", false);
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false}
    ]);
    let inst = env.install_pkg(&pkg, None, grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "en-ng-1")
        .await;
    let snap1 = env.state.indexer().snapshot();
    assert!(
        snap1
            .indexer("plugin:org.example.nogrant/fixture")
            .is_none()
    );
    assert!(
        snap1
            .maintenance("plugin:org.example.nogrant/maint")
            .is_none()
    );

    let rev1 = env.get_plugin_rev(pid).await;
    env.disable_plugin(pid, rev1, "dis-ng-1").await;
    let grant_list = json!([
        {"capability": "indexer:provide", "scope": "Global", "sensitive": false}
    ]);
    let (st_pr, _, b_pr) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/permissions/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({ "requested_grants": grant_list })),
        ))
        .await;
    let prep = assert_ok(st_pr, &b_pr, StatusCode::OK);
    let rev2 = env.get_plugin_rev(pid).await;
    let if_match2 = format!("\"{rev2}\"");
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/permissions/grant"),
            Some(TOKEN),
            &[("idempotency-key", "gr-ng-1"), ("if-match", &if_match2)],
            Some(json!({
                "consent_token": prep["consent_token"],
                "grants": grant_list
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::ACCEPTED);
    let rev3 = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev3, "en-ng-2").await;
    let snap2 = env.state.indexer().snapshot();
    assert!(
        snap2
            .indexer("plugin:org.example.nogrant/fixture")
            .is_some()
    );
    assert!(
        snap2
            .maintenance("plugin:org.example.nogrant/maint")
            .is_none()
    );
}
