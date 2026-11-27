#[path = "builtin_boost/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, MaintenanceVia, Site, SiteId, Torrent};
use indexer::{
    AuthType, FetchMethod, FetchRequest, Fetcher, IndexerError, IndexerProvider, MaintenanceAction,
    MaintenanceCallContext, MaintenanceDescriptor, MaintenanceOutcome, MaintenanceProvider,
    MaintenanceState, ProviderAvailability, ProviderCallContext, ProviderCapability,
    ProviderDescriptor, ProviderSource, RegistryCandidate, RegistrySnapshot, ResolvedDownload,
    SearchQuery,
};
use serde_json::{Value, json};
use store::BoostProtectionState;
use support::plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, req_json};
use support::{BoostTestDownloader, StubTorrentServer, poll_op};

#[derive(Default)]
struct RecordingIntegrationFetcher {
    hits: Mutex<Vec<(FetchMethod, String, Option<String>)>>,
    torrent_srv_port: AtomicUsize,
    demote_on_fetch: Mutex<Option<ApiState>>,
}

impl RecordingIntegrationFetcher {
    fn recorded_urls(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|(_, u, _)| u.clone())
            .collect()
    }

    fn recorded_bodies_for(&self, path_suffix: &str) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, u, _)| u.contains(path_suffix))
            .filter_map(|(_, _, b)| b.clone())
            .collect()
    }
}

impl Fetcher for RecordingIntegrationFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        self.hits
            .lock()
            .unwrap()
            .push((req.method, req.url.clone(), req.body.clone()));
        if let Some(st) = self.demote_on_fetch.lock().unwrap().take() {
            let _ = st.with_instance_writable(false);
        }
        let port = self.torrent_srv_port.load(Ordering::SeqCst);
        if req.url.contains("/api/v1/torrents/genDlToken") {
            let body = req.body.as_deref().unwrap_or("");
            let parsed: Value =
                serde_json::from_str(body).map_err(|e| IndexerError::Parse(e.to_string()))?;
            let id = parsed["id"].as_str().unwrap_or("");
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return Err(IndexerError::Fetch(format!("non-numeric HDArea id: {id}")));
            }
            return Ok(json!({
                "success": true,
                "code": "0",
                "data": { "url": format!("http://127.0.0.1:{port}/hda-{id}.torrent") }
            })
            .to_string());
        }
        if req.url.contains("/api/torrent/genDlToken") {
            let body = req.body.as_deref().unwrap_or("");
            let id = body.strip_prefix("id=").unwrap_or("");
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return Err(IndexerError::Fetch(format!("non-numeric M-Team id: {id}")));
            }
            return Ok(json!({
                "code": "0",
                "message": "SUCCESS",
                "data": format!("http://127.0.0.1:{port}/mt-{id}.torrent")
            })
            .to_string());
        }
        if req.url.contains("/api/v1/torrents") {
            return Ok(json!({
                "success": true,
                "code": "0",
                "data": {
                    "items": [
                        { "id": "101", "name": "Alpha.2026.1080p", "size": "4 GB", "seeders": 12 },
                        { "id": "102", "name": "Beta.2026.2160p", "size": "8 GB", "seeders": 6 }
                    ]
                }
            })
            .to_string());
        }
        Ok("<html><body><a href=\"logout.php\">Logout</a><p>签到成功</p></body></html>".into())
    }
}

struct StubPluginIndexer(ProviderDescriptor);

impl IndexerProvider for StubPluginIndexer {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.0
    }
    fn search(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(Vec::new())
    }
    fn rss(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(Vec::new())
    }
    fn resolve_download(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

struct StubMaintProvider(MaintenanceDescriptor);

impl MaintenanceProvider for StubMaintProvider {
    fn descriptor(&self) -> &MaintenanceDescriptor {
        &self.0
    }
    fn run(
        &self,
        _: &MaintenanceCallContext<'_>,
        _: &Site,
        _: MaintenanceAction,
        _: &str,
    ) -> Result<MaintenanceOutcome, IndexerError> {
        Ok(MaintenanceOutcome {
            state: MaintenanceState::Completed,
            evidence: "stub-ok".into(),
            session_capture: None,
        })
    }
}

fn build_remediation_env(
    srv: Option<&StubTorrentServer>,
) -> (HttpEnv, Arc<RecordingIntegrationFetcher>, DownloaderId) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let fetcher = Arc::new(RecordingIntegrationFetcher::default());
    if let Some(s) = srv {
        fetcher
            .torrent_srv_port
            .store(usize::from(s.addr().port()), Ordering::SeqCst);
    }
    let mut state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        fetcher.clone(),
        dl,
    )
    .unwrap();
    if let Some(s) = srv {
        state = state.with_plugin_http_policy_override(
            net_guard::TargetPolicy::production().with_test_loopback_socket(s.addr()),
        );
    }
    let jobs = state.jobs();
    let queue = jobs.acquire().unwrap();
    for def in queue.list_defs().unwrap() {
        if !def.kind.is_plugin() {
            let _ = queue.set_def_enabled_by_id(def.id, false);
        }
    }
    drop(queue);
    let dl_id = DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "PrimaryDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: Vec::new(),
            is_default: true,
            enabled: true,
        })
        .unwrap();
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    (env, fetcher, dl_id)
}

fn insert_test_site(
    env: &HttpEnv,
    name: &str,
    url: &str,
    profile_id: &str,
    cookie: Option<&str>,
    api_key: Option<&str>,
) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile_id.into(),
            cookie: cookie.map(str::to_string),
            api_key: api_key.map(str::to_string),
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
    id
}

fn register_plugin_indexers(state: &ApiState) {
    let mut cand = RegistryCandidate::from_builtin().unwrap();
    cand = cand
        .add_indexer_provider(Arc::new(StubPluginIndexer(ProviderDescriptor {
            id: "plugin:org.example.custom-idx/tracker".into(),
            name: "CustomTracker".into(),
            kind: "plugin-wasm".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.example.custom-idx".into(),
                component_id: "idx".into(),
                version: "1.0.0".into(),
                plugin_generation: 1,
            },
            auth_types: vec![AuthType::Cookie],
            capabilities: vec![ProviderCapability::Search],
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        })))
        .unwrap()
        .add_indexer_provider(Arc::new(StubPluginIndexer(ProviderDescriptor {
            id: "plugin:org.example.disabled-idx/tracker".into(),
            name: "DisabledTracker".into(),
            kind: "plugin-wasm".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.example.disabled-idx".into(),
                component_id: "idx".into(),
                version: "1.0.0".into(),
                plugin_generation: 1,
            },
            auth_types: vec![AuthType::Cookie],
            capabilities: vec![ProviderCapability::Search],
            availability: ProviderAvailability::Disabled {
                reason: "disabled by admin".into(),
            },
            hidden_from_catalog: false,
        })))
        .unwrap();
    state.indexer().publish(cand).unwrap();
}

#[tokio::test]
async fn verify_site_covers_disabled_nexusphp_plugin_api_hdarea_and_plugin_indexers() {
    let (env, fetcher, _) = build_remediation_env(None);
    let nexus_url = "http://127.0.0.1:19001/";
    let nexus_id = insert_test_site(
        &env,
        "NexusSite",
        nexus_url,
        "pterclub",
        Some("uid=1"),
        None,
    );
    let hdarea_id = insert_test_site(
        &env,
        "HdareaApi",
        "http://127.0.0.1:19002",
        "hdarea",
        None,
        Some("hda-api-token"),
    );
    let plugin_url = "http://127.0.0.1:19003/index";
    let plugin_id = insert_test_site(
        &env,
        "PluginIdxSite",
        plugin_url,
        "plugin:org.example.custom-idx/tracker",
        Some("sid=ok"),
        None,
    );
    let disabled_id = insert_test_site(
        &env,
        "DisabledIdxSite",
        "http://127.0.0.1:19004/",
        "plugin:org.example.disabled-idx/tracker",
        Some("sid=off"),
        None,
    );

    let rev = env.get_plugin_rev("builtin.nexusphp-maintenance").await;
    env.disable_plugin("builtin.nexusphp-maintenance", rev, "dis-nexus-maint")
        .await;
    register_plugin_indexers(&env.state);

    let (st_nex, _, b_nex) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{nexus_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let d_nex = assert_ok(st_nex, &b_nex, StatusCode::OK);
    assert_eq!(d_nex["ok"], false);
    assert!(
        d_nex["error"]
            .as_str()
            .unwrap_or("")
            .contains("内置 NexusPHP 维护插件已停用")
    );
    assert!(
        !fetcher.recorded_urls().iter().any(|u| u == nexus_url),
        "disabled NexusPHP maintenance must not fall back to raw GET site.url"
    );

    let (st_hda, _, b_hda) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{hdarea_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let d_hda = assert_ok(st_hda, &b_hda, StatusCode::OK);
    assert_eq!(d_hda["ok"], true, "d_hda={d_hda}");
    assert_eq!(d_hda["torrent_count"], 2);

    let (st_plg, _, b_plg) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{plugin_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let d_plg = assert_ok(st_plg, &b_plg, StatusCode::OK);
    assert_eq!(d_plg["ok"], true, "d_plg={d_plg}");
    assert_eq!(d_plg["torrent_count"], 0);
    assert!(fetcher.recorded_urls().iter().any(|u| u == plugin_url));

    let (st_dis, _, b_dis) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{disabled_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let d_dis = assert_ok(st_dis, &b_dis, StatusCode::OK);
    assert_eq!(d_dis["ok"], false);
    assert!(d_dis["error"].as_str().unwrap_or("").contains("已停用"));
}

#[tokio::test]
async fn verify_site_enforces_foreground_write_admission_before_and_during_io() {
    let (env, fetcher, _) = build_remediation_env(None);
    let site_id = insert_test_site(
        &env,
        "ForegroundGateSite",
        "http://127.0.0.1:19010/",
        "pterclub",
        Some("uid=1; pass=ok"),
        None,
    );

    let _ = env.state.clone().with_instance_writable(false);
    let (st_ro, _, b_ro) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_ro, &b_ro, StatusCode::CONFLICT, "instance.read_only");

    let _ = env.state.clone().with_instance_writable(true);
    *fetcher.demote_on_fetch.lock().unwrap() = Some(env.state.clone());
    let (st_mid, _, b_mid) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{site_id}/verify"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(st_mid, &b_mid, StatusCode::CONFLICT, "instance.read_only");
    let receipts = env
        .state
        .store()
        .list_site_maintenance_receipts(
            &store::SiteMaintenanceReceiptFilter {
                site_id: Some(site_id),
                action: Some(store::SiteMaintenanceAction::VerifySession),
                state: Some(store::SiteMaintenanceState::Completed),
            },
            None,
            Some(10),
        )
        .unwrap();
    assert!(
        receipts.items.is_empty(),
        "post-IO receipt finalization must be blocked after read-only demotion"
    );
}

async fn submit_boost_command(env: &HttpEnv, payload: Value, idem: &str) -> (StatusCode, Value) {
    let rev = env
        .state
        .store()
        .get_plugin("cm.boost")
        .unwrap()
        .unwrap()
        .revision;
    let (st, _, b) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{rev}\"")),
                ("Idempotency-Key", idem),
            ],
            Some(json!({ "kind": "downloader.add", "payload": payload })),
        ))
        .await;
    (st, b)
}

#[tokio::test]
async fn downloader_add_hdarea_and_mteam_extract_numeric_id_and_honor_hr_protection() {
    let srv = StubTorrentServer::start_dynamic(2048);
    let (env, fetcher, dl_id) = build_remediation_env(Some(&srv));
    support::plugin_http_support::plugins_fixture::install_cm_boost(&env, false, false, false)
        .await;
    support::plugin_http_support::plugins_fixture::save_plugin_config_direct(
        &env.state,
        "cm.boost",
        json!({ "hold_days": 0, "budget_bytes": 100_000 }),
    );
    let srv_origin = format!("http://{}", srv.addr());
    let hda_id = insert_test_site(
        &env,
        "HdareaDl",
        &srv_origin,
        "hdarea",
        None,
        Some("hda-dl-key"),
    );
    let mt_id = insert_test_site(
        &env,
        "MteamDl",
        &srv_origin,
        "mteam",
        None,
        Some("mt-dl-key"),
    );

    let (st_bad, b_bad) = submit_boost_command(
        &env,
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": "not-a-valid-uuid",
            "download_url": "https://hdarea.club/download.php?id=54321"
        }),
        "dl-bad-site",
    )
    .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    let (st_hda, b_hda) = submit_boost_command(
        &env,
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": hda_id.to_string(),
            "download_url": "https://hdarea.club/download.php?id=54321",
            "title": "Hdarea.HR.2026",
            "hr": true
        }),
        "dl-hda-1",
    )
    .await;
    let op_hda = poll_op(
        &env,
        assert_ok(st_hda, &b_hda, StatusCode::ACCEPTED)["operation_id"]
            .as_str()
            .unwrap(),
    )
    .await;
    assert_eq!(op_hda["state"], "succeeded", "op_hda={op_hda}");
    assert_eq!(
        fetcher.recorded_bodies_for("/api/v1/torrents/genDlToken"),
        vec![r#"{"id":"54321"}"#.to_string()]
    );

    let (st_mt, b_mt) = submit_boost_command(
        &env,
        json!({
            "downloader_id": dl_id.to_string(),
            "site_id": mt_id.to_string(),
            "download_url": "https://api.m-team.cc/api/torrent/download?id=98765",
            "title": "MTeam.NoHR.2026"
        }),
        "dl-mt-1",
    )
    .await;
    let op_mt = poll_op(
        &env,
        assert_ok(st_mt, &b_mt, StatusCode::ACCEPTED)["operation_id"]
            .as_str()
            .unwrap(),
    )
    .await;
    assert_eq!(op_mt["state"], "succeeded", "op_mt={op_mt}");
    assert_eq!(
        fetcher.recorded_bodies_for("/api/torrent/genDlToken"),
        vec!["id=98765".to_string()]
    );

    let facts = env.state.store().list_active_boost_task_facts().unwrap();
    let hda_fact = facts.iter().find(|f| f.site_id == Some(hda_id)).unwrap();
    assert_eq!(hda_fact.hr_active, Some(true));
    assert_eq!(hda_fact.protection_state, BoostProtectionState::Protected);
    assert_eq!(hda_fact.protection_reason.as_deref(), Some("hr-active"));
    let mt_fact = facts.iter().find(|f| f.site_id == Some(mt_id)).unwrap();
    assert_eq!(mt_fact.hr_active, Some(false));
    assert_eq!(mt_fact.protection_state, BoostProtectionState::Eligible);
}

struct SigninStubServer {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SigninStubServer {
    fn start(expected_api_key: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_c = hits.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                        let ok_req = req.starts_with("post /api/v1/user/signin ")
                            && req.contains(&format!("x-api-key: {expected_api_key}"));
                        let (status, body) = if ok_req {
                            hits_c.fetch_add(1, Ordering::SeqCst);
                            (
                                "200 OK",
                                r#"{"success":true,"code":0,"message":"签到成功"}"#,
                            )
                        } else {
                            (
                                "401 Unauthorized",
                                r#"{"success":false,"code":401,"message":"bad key"}"#,
                            )
                        };
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            hits,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for SigninStubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn publish_yaml_plugin_with_maint(state: &ApiState, maint_enabled: bool) {
    let yaml = "id: customapi\nframework: api\nbase_url: https://custom.example\nsearch:\n  path: /api/v1/torrents\n  query_param: q\n";
    let availability = if maint_enabled {
        ProviderAvailability::Enabled
    } else {
        ProviderAvailability::Disabled {
            reason: "disabled".into(),
        }
    };
    let cand = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.yaml-maint", "1.0.0", 1, yaml)
        .unwrap()
        .add_maintenance_provider(Arc::new(StubMaintProvider(MaintenanceDescriptor {
            id: "plugin:org.example.yaml-maint/maint".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.example.yaml-maint".into(),
                component_id: "maint".into(),
                version: "1.0.0".into(),
                plugin_generation: 1,
            },
            actions: vec![MaintenanceAction::CheckIn],
            availability,
        })))
        .unwrap();
    state.indexer().publish(cand).unwrap();
}

#[tokio::test]
async fn hdarea_api_key_check_in_via_plugin_actions_and_checkin_command_plus_catalog() {
    let signin_srv = SigninStubServer::start("hda-live-key");
    let (env, _, _) = build_remediation_env(None);
    let hda_id = insert_test_site(
        &env,
        "HdareaCheckin",
        &format!("http://{}", signin_srv.addr),
        "hdarea",
        None,
        Some("hda-live-key"),
    );
    let site_rev = env
        .state
        .store()
        .observe_site(hda_id)
        .unwrap()
        .unwrap()
        .revision();

    let (st_act, _, b_act) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/sites/{hda_id}/plugin-actions"),
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{site_rev}\"")),
                ("Idempotency-Key", "hda-pa-1"),
            ],
            Some(json!({ "action": "check-in" })),
        ))
        .await;
    let op_act_id = assert_ok(st_act, &b_act, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let op_act = poll_op(&env, &op_act_id).await;
    assert_eq!(op_act["state"], "succeeded", "op_act={op_act}");
    assert_eq!(signin_srv.hits.load(Ordering::SeqCst), 1);

    support::plugin_http_support::plugins_fixture::install_cm_checkin(
        &env, false, false, false, false,
    )
    .await;
    let checkin_rev = env.get_plugin_rev("cm.checkin").await;
    let (st_cmd, _, b_cmd) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/commands",
            Some(TOKEN),
            &[
                ("If-Match", &format!("\"{checkin_rev}\"")),
                ("Idempotency-Key", "hda-cmd-1"),
            ],
            Some(json!({
                "kind": "site.maintenance",
                "payload": { "site_id": hda_id.to_string(), "action": "check-in" }
            })),
        ))
        .await;
    let op_cmd_id = assert_ok(st_cmd, &b_cmd, StatusCode::ACCEPTED)["operation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let op_cmd = poll_op(&env, &op_cmd_id).await;
    assert_eq!(op_cmd["state"], "succeeded", "op_cmd={op_cmd}");
    assert_eq!(signin_srv.hits.load(Ordering::SeqCst), 2);

    publish_yaml_plugin_with_maint(&env.state, true);
    let (st_cat1, _, b_cat1) = env
        .send(req_json(
            "GET",
            "/api/v1/sites/catalog",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cat1 = assert_ok(st_cat1, &b_cat1, StatusCode::OK);
    let entry1 = cat1
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["profile_id"] == "plugin:org.example.yaml-maint/customapi")
        .unwrap();
    assert_eq!(entry1["supports_check_in"], true);

    publish_yaml_plugin_with_maint(&env.state, false);
    let (st_cat2, _, b_cat2) = env
        .send(req_json(
            "GET",
            "/api/v1/sites/catalog",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cat2 = assert_ok(st_cat2, &b_cat2, StatusCode::OK);
    let entry2 = cat2
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["profile_id"] == "plugin:org.example.yaml-maint/customapi")
        .unwrap();
    assert_eq!(entry2["supports_check_in"], false);
}
