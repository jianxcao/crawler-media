#[path = "builtin_boost/support.rs"]
pub(crate) mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use api::Store;
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{
    Browser, BrowserConfig, FetchRequest, Fetcher, IndexerError, PluginBrowserError,
    PluginBrowserPage, PluginNavResponse, RegistrySnapshot,
};
use plugin_api::{BrowserFillValue, BrowserPurpose, ErrorCode, GuestRole, PluginTrust};
use plugin_host::Invocation;
use plugin_package::build_deterministic_cmplugin_zip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use store::{
    AcceptDownloadOperationInput, BoostProtectionState, BoostTaskState, DownloadOwnershipState,
    DownloadProvenanceKind, ManualCredentialWrite, RecordDownloadOwnershipInput,
    UpsertBoostTaskFactInput,
};
use support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok,
    pkg::{TestSigningKey, build_stored_zip},
    req_json,
};
use uuid::Uuid;

const INDEXER_WASM: &[u8] = include_bytes!("../../plugin-host/tests/fixtures/indexer_fixture.wasm");

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct XhtmlLiveFencePage;

impl PluginBrowserPage for XhtmlLiveFencePage {
    fn set_cookies(&mut self, _: &str, _: &str) -> Result<(), PluginBrowserError> {
        Ok(())
    }
    fn navigate(&mut self, _: &str, _: &str) -> Result<PluginNavResponse, PluginBrowserError> {
        Ok(PluginNavResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/xhtml+xml".into())],
            body: br#"<x:html xmlns:x="http://www.w3.org/1999/xhtml"><x:body><x:input id="tok" type="PASSWORD" value="XHTML_NAV_PW"/></x:body></x:html>"#.to_vec(),
        })
    }
    fn eval_json(&mut self, expr: &str) -> Result<Value, PluginBrowserError> {
        if expr.contains("__cm_origin") {
            return Ok(json!("https://pt-r12.example/login"));
        }
        if expr.contains("__cm_dom") {
            return Ok(json!({
                "html": r#"<x:html xmlns:x="http://www.w3.org/1999/xhtml"><x:body><x:script>evilXhtmlDom()</x:script><x:input id="tok" type="PASSWORD" value="XHTML_DOM_PW"/></x:body></x:html>"#,
                "xml": true
            }));
        }
        if expr.contains("__cm_query") {
            return Ok(json!([{ "handle": "cmh-1", "visible_text": "" }]));
        }
        Ok(json!(true))
    }
    fn cookie_header(&mut self, _: &str) -> Result<String, PluginBrowserError> {
        Ok("uid=42".into())
    }
    fn close_verified(&mut self) -> Result<(), PluginBrowserError> {
        Ok(())
    }
}

fn build_r12_indexer_pkg(pid: &str) -> Vec<u8> {
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R12B\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"t\"]\ndescription = \"d\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\ncredential_binding = \"invocation-site\"\n[[components]]\nid = \"fixture\"\nentry = \"indexer.wasm\"\nworld = \"crawler:plugin/indexer-plugin@1.0.0\"\npermissions = [\"http\", \"http:site-auth\", \"read:sites\", \"browser:search\", \"indexer:provide\"]\n[[components.indexers]]\nid = \"fixture\"\nname = \"Fixture\"\nkind = \"pt\"\ncapabilities = [\"search\", \"rss\", \"detail\", \"resolve-download\", \"user-stats\", \"categories\"]\nsite_schema = \"site.schema.json\"\n"
    );
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{}}"#;
    build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("site.schema.json", schema.as_bytes()),
        ("indexer.wasm", INDEXER_WASM),
    ])
}

fn insert_site(store: &Store, name: &str, url: &str, profile: &str, enabled: bool) -> SiteId {
    let id = SiteId::new();
    store
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile.into(),
            cookie: Some("c_uid=1".into()),
            api_key: Some("INIT_API_KEY".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: Some("http://127.0.0.1:19222".into()),
            downloader_id: None,
            enabled,
            maintenance_via: MaintenanceVia::Browser,
            browser_search: true,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

#[tokio::test]
async fn live_browser_session_fences_site_disable_epoch_rotation_and_prefixed_xhtml_dom() {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(1201);
    env.pin_trust_key(&key, "r12-bkey").await;
    let pid = "org.example.r12-browser-live-fence";
    let pkg_bytes = build_r12_indexer_pkg(pid);
    let sig = key.sign_package(&pkg_bytes);
    let store = env.state.store();
    store.put_setting("cdp.enabled", "true").unwrap();
    let site_id = insert_site(
        &store,
        "R12Site",
        "https://pt-r12.example/",
        &format!("plugin:{pid}/fixture"),
        true,
    );
    let grants = json!([
        {"capability": "http", "scope": "Global", "sensitive": false},
        {"capability": "read:sites", "scope": "Global", "sensitive": false},
        {"capability": "http:site-auth", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "browser:search", "scope": {"Site": site_id.to_string()}, "sensitive": true},
        {"capability": "indexer:provide", "scope": {"Site": site_id.to_string()}, "sensitive": true}
    ]);
    let inst = env.install_pkg(&pkg_bytes, Some(&sig), grants).await;
    env.enable_plugin(pid, inst["revision"].as_i64().unwrap(), "r12-bf-en")
        .await;

    let browser = Arc::new(
        Browser::new(BrowserConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:19222".into()),
            managed_endpoint: None,
        })
        .with_min_work(Duration::from_millis(10))
        .with_queue_cap(Duration::from_secs(2))
        .with_plugin_opener(|_| Ok(Box::new(XhtmlLiveFencePage))),
    );
    let state_t = env
        .state
        .clone()
        .with_browser(browser)
        .with_admin_trusted_plugins([pid]);
    let ports = state_t.plugins().host_ports();
    let ep = ports
        .site_credential_epoch(&site_id.to_string())
        .unwrap()
        .unwrap();
    let mut inv = Invocation::new(pid, Duration::from_secs(5));
    inv.generation = 1;
    inv.role = GuestRole::Indexer;
    inv.site_id = Some(site_id.to_string());
    inv.credential_epoch = Some(ep);
    inv.trust = PluginTrust::AdminTrusted;
    let _g = ports.begin_invocation(&inv).unwrap();
    let mut sess = ports.browser_open(&inv, BrowserPurpose::Search).unwrap();
    sess.navigate("/login", "load").unwrap();
    let els = sess.query("#tok", 1).unwrap();
    let dom = sess.dom(4096).unwrap();
    assert!(
        !dom.contains("XHTML_DOM_PW"),
        "prefixed XHTML leaked pw: {dom}"
    );
    assert!(
        !dom.contains("evilXhtmlDom"),
        "prefixed XHTML leaked script: {dom}"
    );

    let mut rotated = store.get_site(site_id).unwrap().unwrap();
    rotated.api_key = Some("ROTATED_API_KEY".into());
    store
        .save_site(&rotated, ManualCredentialWrite::Replace)
        .unwrap();

    let dom_err = sess.dom(4096).expect_err("rotated epoch must fence dom()");
    assert_eq!(dom_err.code, ErrorCode::StaleGeneration, "{dom_err:?}");
    let fill_err = sess
        .fill(
            &els[0].handle,
            BrowserFillValue::CredentialSlot("site-api-key".into()),
        )
        .expect_err("rotated epoch must fence fill(CredentialSlot)");
    assert_eq!(fill_err.code, ErrorCode::StaleGeneration, "{fill_err:?}");
}

struct DrainBoostOnFetch {
    store: Arc<Store>,
}

impl Fetcher for DrainBoostOnFetch {
    fn fetch(&self, _req: &FetchRequest) -> Result<String, IndexerError> {
        if let Ok(Some(rec)) = self.store.get_plugin("builtin.boost")
            && rec.lifecycle == store::plugins::PluginLifecycle::Enabled
        {
            let _ = self.store.begin_plugin_lifecycle(
                "builtin.boost",
                rec.revision,
                Uuid::new_v4(),
                false,
            );
        }
        Err(IndexerError::Fetch("simulated empty search".into()))
    }
}

type CheckinStub = (
    std::net::SocketAddr,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
);

fn spawn_checkin_stub() -> CheckinStub {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_bg = hits.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_c = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_c.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    hits_bg.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = s.set_nonblocking(false);
                    let mut buf = [0u8; 1024];
                    let _ = std::io::Read::read(&mut s, &mut buf);
                    let body = "<html><body>这是您的第 1 次签到</body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = std::io::Write::write_all(&mut s, resp.as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (addr, hits, stop, handle)
}

fn seed_retained_boost_file(
    store: &Store,
    site_id: SiteId,
    tmp: &std::path::Path,
    target_file: &std::path::Path,
) -> DownloaderId {
    let dl_id = DownloaderId::new();
    store
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "DelFenceDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:8080".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![downloader::PathMap {
                from: tmp.to_string_lossy().into_owned(),
                to: tmp.to_path_buf(),
            }],
            is_default: true,
            enabled: true,
        })
        .unwrap();
    let tf_id = support::seed_boost_task(
        store,
        dl_id,
        site_id,
        "r12-del",
        &"a".repeat(40),
        BoostTaskState::Seeding,
        100,
        0,
        Some(false),
        None,
    );
    let plan_json = json!({
        "save_path": tmp,
        "allowed_roots": [tmp],
        "paths": [target_file]
    })
    .to_string();
    store
        .mark_boost_task_retained_with_paths(&tf_id, 1, 8, Some(&plan_json), 1_700_000_010)
        .unwrap();
    dl_id
}

fn save_checkin_config_and_enable_task(
    store: &Store,
    site_id: SiteId,
) -> domain::PluginTaskBindingId {
    let cur = store.get_plugin("builtin.checkin").unwrap().unwrap();
    let dis = store
        .begin_plugin_lifecycle("builtin.checkin", cur.revision, Uuid::new_v4(), false)
        .unwrap();
    let dis = store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: dis.plugin_id.clone(),
            operation_id: dis.active_operation_id.unwrap(),
            revision: dis.revision,
            generation: dis.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    let saved = store
        .save_plugin_config(
            "builtin.checkin",
            dis.revision,
            "1",
            &json!({ "site_id": site_id.to_string() }),
        )
        .unwrap();
    let en = store
        .begin_plugin_lifecycle("builtin.checkin", saved.revision, Uuid::new_v4(), true)
        .unwrap();
    let en = store
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: en.plugin_id.clone(),
            operation_id: en.active_operation_id.unwrap(),
            revision: en.revision,
            generation: en.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap();
    let cur_bind = store
        .get_plugin_task_binding("builtin.checkin", "check-in")
        .unwrap()
        .unwrap();
    let bind = store
        .save_plugin_task_binding(
            &en,
            "check-in",
            cur_bind.definition_revision,
            &store::plugins::PluginTaskDefinition {
                enabled: true,
                interval_secs: Some(86_400),
                public_config: json!({}),
            },
        )
        .unwrap();
    domain::PluginTaskBindingId::from_uuid(bind.binding_id)
}

async fn assert_disabled_site_checkin_rejected(
    env: &HttpEnv,
    store: &Store,
    checkin_addr: std::net::SocketAddr,
) {
    let disabled_site = SiteId::new();
    store
        .insert_site(&Site {
            id: disabled_site,
            name: "DisabledCheckin".into(),
            url: format!("http://{checkin_addr}/"),
            profile_id: "pterclub".into(),
            cookie: Some("c_uid=1".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: false,
            maintenance_via: MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    let b_id = save_checkin_config_and_enable_task(store, disabled_site);
    let task_job = jobs::Job {
        id: domain::JobId::new(),
        def_id: None,
        kind: jobs::JobKind::PluginTask,
        scope: jobs::JobScope::PluginTask(b_id),
        status: jobs::JobStatus::Running,
        attempt: 1,
        payload: json!({
            "binding_id": b_id,
            "plugin_id": "builtin.checkin",
            "task_id": "check-in",
            "input": {}
        })
        .to_string(),
        run_after: 0,
        started_at: Some(1_700_000_000),
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: None,
        timeout_secs: Some(60),
    };
    let task_err = tokio::task::spawn_blocking({
        let st = env.state.clone();
        move || api::plugins::run_task_job(&st, &task_job)
    })
    .await
    .unwrap()
    .expect_err("disabled site_id must fail builtin.checkin task");
    assert!(task_err.contains("disabled"), "{task_err}");
}

#[tokio::test]
async fn boost_cycle_and_checkin_recheck_live_plugin_and_site_state() {
    let (checkin_addr, checkin_hits, stop_bg, srv_thread) = spawn_checkin_stub();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let fetcher = Arc::new(DrainBoostOnFetch {
        store: store.clone(),
    });
    let dl_mem = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let state = api::ApiState::new_arc_with_admin_password(
        store.clone(),
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        fetcher,
        dl_mem,
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(checkin_addr),
    );
    let env = HttpEnv {
        _dir: dir,
        app: api::router(state.clone()),
        state,
        member_token: String::new(),
    };

    assert_disabled_site_checkin_rejected(&env, &store, checkin_addr).await;
    stop_bg.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = srv_thread.join();
    assert_eq!(checkin_hits.load(std::sync::atomic::Ordering::SeqCst), 0);

    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let target_file = tmp.path().join("keep.mkv");
    std::fs::write(&target_file, b"precious").unwrap();
    let ok_site = insert_site(
        &store,
        "ActivePT",
        "https://active-pt.example/",
        "pterclub",
        true,
    );
    let dl_id = seed_retained_boost_file(&store, ok_site, tmp.path(), &target_file);
    support::save_boost_config(
        &env.state,
        json!({
            "downloader_id": dl_id.to_string(),
            "sites": [ok_site.to_string()],
            "auto_cleanup_files": true,
            "dry_run": false
        }),
    );
    let boost_rev = env.get_plugin_rev("builtin.boost").await;
    let (st_b, _, b_b) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{boost_rev}\"")),
                ("idempotency-key", "r12-boost-mid-drain"),
            ],
            Some(json!({ "kind": "boost.run", "payload": {} })),
        ))
        .await;
    let boost_op = assert_ok(st_b, &b_b, StatusCode::ACCEPTED);
    let op_id = boost_op["operation_id"].as_str().unwrap();
    for _ in 0..20 {
        let _ = env
            .send(req_json(
                "POST",
                "/api/v1/jobs/tick",
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let (st_o, _, b_o) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugin-operations/{op_id}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        let cur = assert_ok(st_o, &b_o, StatusCode::OK);
        if !matches!(cur["state"].as_str(), Some("queued" | "running")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(target_file.exists());
}

fn build_r12_ui_dl_pkg(pid: &str) -> Vec<u8> {
    let remote_js = b"export const remote = true;\n";
    let asset_manifest = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{
            "path": "ui/assets/remoteEntry.js",
            "sha256": sha256_hex(remote_js),
            "size": remote_js.len(),
            "content_type": "application/javascript"
        }]
    }))
    .unwrap();
    let caps = json!([
        "read:boost",
        "command:downloader.delete-task",
        "command:downloader.control",
        "command:downloader.prepare-cleanup"
    ]);
    let manifest = format!(
        "manifest_version = 1\nid = \"{pid}\"\nname = \"R12UI\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"T\"]\ndescription = \"r12\"\nconfig_schema = \"schemas/config.schema.json\"\nstate_schema = 1\n[ui]\nkind = \"federation-react\"\ncontract = \"1.0.0\"\nreact = \"19.3.0\"\nreact_dom = \"19.3.0\"\nremote_name = \"r12ui\"\nentry = \"ui/assets/remoteEntry.js\"\nasset_manifest = \"ui/asset-manifest.json\"\ncapabilities = {caps}\n[[ui.surfaces]]\nkind = \"page\"\nid = \"main\"\nexpose = \"./Page\"\ntitle = \"R12\"\n[[components]]\nid = \"worker\"\nentry = \"components/worker.wasm\"\nworld = \"{}\"\npermissions = {caps}\n[[components.tasks]]\nid = \"sync\"\ninput_schema = \"schemas/task.schema.json\"\nschedule = {{ kind = \"interval\", seconds = 3600 }}\n",
        plugin_host::TASK_WORLD,
    );
    let mut entries = BTreeMap::new();
    entries.insert("plugin.toml".into(), manifest.into_bytes());
    entries.insert(
        "schemas/config.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{}}"#.to_vec(),
    );
    entries.insert(
        "schemas/task.schema.json".into(),
        br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#.to_vec(),
    );
    entries.insert(
        "components/worker.wasm".into(),
        support::plugin_http_support::pkg::task_wasm().to_vec(),
    );
    entries.insert("ui/asset-manifest.json".into(), asset_manifest);
    entries.insert("ui/assets/remoteEntry.js".into(), remote_js.to_vec());
    build_deterministic_cmplugin_zip(&entries).unwrap()
}

async fn grant_ui_trust(env: &HttpEnv, pid: &str) {
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/prepare"),
            Some(TOKEN),
            &[],
            Some(json!({})),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let rev = prep["trust_revision"].as_i64().unwrap_or(0);
    let reauth = env.reauth("plugin-ui-trust").await;
    let (st_g, _, b_g) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui-trust/grant"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", &format!("ug-{}", Uuid::new_v4())),
            ],
            Some(json!({
                "preparation_id": prep["preparation_id"],
                "package_hash": prep["package_hash"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            })),
        ))
        .await;
    assert_ok(st_g, &b_g, StatusCode::OK);
}

fn seed_ui_owned_task(store: &Store, pid: &str, dl_id: DownloaderId, site_id: SiteId, hash: &str) {
    let now = 1_700_000_000i64;
    store
        .accept_download_operation(&AcceptDownloadOperationInput {
            operation_id: "op-r12-own".into(),
            idempotency_key: "ik-r12-own".into(),
            source_kind: "boost".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            cause_id: "seed".into(),
            site_id: Some(site_id),
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            enclosure: "https://pt.example/1.torrent".into(),
            ownership_tag: "cm-own-r12-ui".into(),
            info_hash: Some(hash.into()),
            bytes_digest: Some("dig-r12".into()),
            verified_bytes: Some(1024),
            request_digest: "req-r12".into(),
            now_utc: now,
        })
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: "cm-own-r12-ui".into(),
            info_hash: Some(hash.into()),
            operation_id: "op-r12-own".into(),
            subscribe_id: None,
            plugin_id: Some(pid.into()),
            subject: pid.into(),
            site_id: Some(site_id),
            enclosure: "https://pt.example/1.torrent".into(),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(1024),
            bytes_digest: Some("dig-r12".into()),
            now_utc: now,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-r12-ui".into(),
            plugin_id: pid.into(),
            quota_key: format!("{pid}:{dl_id}"),
            downloader_id: dl_id,
            ownership_tag: "cm-own-r12-ui".into(),
            info_hash: hash.into(),
            site_id: Some(site_id),
            state: BoostTaskState::Seeding,
            occupied_bytes: 1024,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: now,
            expected_revision: None,
            now_utc: now,
        })
        .unwrap();
}

#[tokio::test]
async fn ui_attributed_commands_forbid_raw_targets_and_shadow_fields_and_infer_downloader_from_task_handles()
 {
    let env = HttpEnv::new().await;
    let key = TestSigningKey::generate(1203);
    env.pin_trust_key(&key, "k-1203").await;
    let store = env.state.store();
    let dl_id = env.insert_downloader("ScopedDL");
    let site_id = env.insert_site("ScopedSite");

    let pid = "org.example.r12.uicmd";
    let pkg = build_r12_ui_dl_pkg(pid);
    let pkg_hash = sha256_hex(&pkg);
    env.install_pkg(
        &pkg,
        Some(&key.sign_package(&pkg)),
        json!([
            { "capability": "read:boost", "scope": "Global", "sensitive": false },
            { "capability": "command:downloader.delete-task", "scope": { "Object": { "kind": "downloader", "id": dl_id.to_string() } }, "sensitive": false },
            { "capability": "command:downloader.control", "scope": { "Object": { "kind": "downloader", "id": dl_id.to_string() } }, "sensitive": false },
            { "capability": "command:downloader.prepare-cleanup", "scope": { "Object": { "kind": "downloader", "id": dl_id.to_string() } }, "sensitive": false }
        ]),
    )
    .await;
    let rev = env.get_plugin_rev(pid).await;
    env.enable_plugin(pid, rev, "en-r12-uicmd").await;
    grant_ui_trust(&env, pid).await;

    let info_hash = "b".repeat(40);
    seed_ui_owned_task(&store, pid, dl_id, site_id, &info_hash);

    let (st_h, _, b_h) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/ui/query"),
            Some(TOKEN),
            &[("x-plugin-package", &pkg_hash), ("x-plugin-generation", "1")],
            Some(json!({ "name": "handles.resolve", "input": { "kind": "owned_download", "id": "tf-r12-ui" } })),
        ))
        .await;
    let task_handle = assert_ok(st_h, &b_h, StatusCode::OK)["handle"]
        .as_str()
        .unwrap()
        .to_string();

    for (idem, body) in [
        (
            "r12-ui-explicit-targets",
            json!({
                "kind": "downloader.control",
                "targets": [{ "kind": "downloader", "id": dl_id.to_string() }],
                "payload": { "owned_task_handle": task_handle, "action": "pause" }
            }),
        ),
        (
            "r12-ui-shadow-infohash",
            json!({
                "kind": "downloader.delete-task",
                "payload": { "owned_task_handle": task_handle, "info_hash": info_hash }
            }),
        ),
        (
            "r12-ui-prep-raw-tid",
            json!({
                "kind": "downloader.prepare-cleanup",
                "payload": { "owned_task_handles": [task_handle], "task_id": "tf-r12-ui" }
            }),
        ),
    ] {
        let (st, _, b) = env
            .send(req_json(
                "POST",
                &format!("/api/v1/plugins/{pid}/commands"),
                Some(TOKEN),
                &[
                    ("idempotency-key", idem),
                    ("x-plugin-package", &pkg_hash),
                    ("x-plugin-generation", "1"),
                ],
                Some(body),
            ))
            .await;
        assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.invalid_handle");
    }

    let cur_rev = env.get_plugin_rev(pid).await;
    let (st_ctl, _, b_ctl) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugins/{pid}/commands"),
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{cur_rev}\"")),
                ("idempotency-key", "r12-adm-ctl-hashes"),
            ],
            Some(json!({
                "kind": "downloader.control",
                "payload": { "hashes": [info_hash.to_uppercase()], "action": "pause" }
            })),
        ))
        .await;
    assert_eq!(
        st_ctl,
        StatusCode::ACCEPTED,
        "hashes target inference failed: {b_ctl}"
    );
}
