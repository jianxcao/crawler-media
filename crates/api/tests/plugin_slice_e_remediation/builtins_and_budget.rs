use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, Site, SiteId, Torrent};
use indexer::{IndexerError, RegistrySnapshot};
use serde_json::json;
use store::SiteMaintenanceReceiptFilter;
use store::plugins::{PluginGrant, PluginGrantScope};

use super::replace_plugin_grants;
use super::support::plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok,
    plugins_fixture::{install_cm_boost, install_cm_checkin, save_plugin_config_direct},
    req_json,
};
use super::support::{BoostTestDownloader, NoopFetcher, StubTorrentServer, poll_op};

struct MiniAttendanceServer {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MiniAttendanceServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 1024];
                        let _ = s.read(&mut buf);
                        let body = "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.write_all(hdr.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }
}

impl Drop for MiniAttendanceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct RecordingSearchProvider {
    seen_sites: Arc<StdMutex<Vec<SiteId>>>,
    enclosure_url: String,
    desc: indexer::ProviderDescriptor,
}

impl indexer::IndexerProvider for RecordingSearchProvider {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.desc
    }

    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        site: &Site,
        _: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.seen_sites.lock().unwrap().push(site.id);
        Ok(vec![Torrent {
            site_id: site.id,
            id: Some("cand-1".into()),
            title: "Cand.8K".into(),
            enclosure: self.enclosure_url.clone(),
            size_bytes: Some(8192),
            seeders: Some(10),
            leechers: Some(1),
            snatched: None,
            upload_time: None,
            poster_url: None,
            free: true,
            hr: Some(false),
            imdb_id: None,
            detail_url: None,
            category: None,
            free_2x: false,
            free_until: None,
        }])
    }

    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }

    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

#[tokio::test]
async fn builtin_checkin_and_boost_enforce_config_site_id_grants_and_budget_cap() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let att_srv = MiniAttendanceServer::start();
    let tor_srv = StubTorrentServer::start_dynamic(8192);
    let dl = Arc::new(BoostTestDownloader::new(dir.path().join("stage")));
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let seen_sites = Arc::new(StdMutex::new(Vec::new()));
    let state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl,
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production()
            .with_test_loopback_socket(att_srv.addr())
            .with_test_loopback_socket(tor_srv.addr()),
    );
    {
        let jobs = state.jobs();
        let queue = jobs.acquire().unwrap();
        for def in queue.list_defs().unwrap() {
            if !def.kind.is_plugin() {
                let _ = queue.set_def_enabled_by_id(def.id, false);
            }
        }
    }
    let env = HttpEnv {
        _dir: dir,
        app: router(state.clone()),
        state,
        member_token: String::new(),
    };
    install_cm_checkin(&env, false, false, false, false).await;
    install_cm_boost(&env, false, false, false).await;

    let insert_site = |name: &str, profile: &str, url: String, cookie: Option<&str>| {
        let id = SiteId::new();
        env.state
            .store()
            .insert_site(&Site {
                id,
                name: name.into(),
                url,
                profile_id: profile.into(),
                cookie: cookie.map(Into::into),
                api_key: None,
                rss_url: None,
                proxy: None,
                rate_limit_per_minute: Some(60),
                cdp_url: None,
                downloader_id: None,
                enabled: true,
                maintenance_via: domain::MaintenanceVia::Http,
                browser_search: false,
                timezone: "UTC".into(),
            })
            .unwrap();
        id
    };
    let (site_a, site_b) = (
        insert_site("Checkin-A", "hdarea", att_srv.url(), Some("c_secure_uid=1")),
        insert_site("Checkin-B", "hdarea", att_srv.url(), Some("c_secure_uid=1")),
    );
    assert_checkin_config_and_grants(&env, site_a, site_b).await;

    let reg = indexer::RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(RecordingSearchProvider {
            seen_sites: seen_sites.clone(),
            enclosure_url: tor_srv.url("big.torrent"),
            desc: indexer::ProviderDescriptor {
                id: "slice_e_idx".into(),
                name: "SliceEIdx".into(),
                kind: "builtin".into(),
                source: indexer::ProviderSource::Builtin,
                auth_types: vec![indexer::AuthType::Cookie],
                capabilities: vec![indexer::ProviderCapability::Search],
                availability: indexer::ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
        }))
        .unwrap();
    env.state.indexer().publish(reg).unwrap();
    let insert_boost_site =
        |name: &str| insert_site(name, "slice_e_idx", "https://pt.example/".into(), None);
    assert_boost_grants_and_budget_cap(&env, &insert_boost_site, &seen_sites).await;
}

async fn configure_checkin_site_a_and_enable_task(env: &HttpEnv, site_a: SiteId) -> i64 {
    let rev = env.get_plugin_rev("cm.checkin").await;
    env.disable_plugin("cm.checkin", rev, "chk-dis-e").await;
    let (_, _, cfg_body) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.checkin/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let cfg = assert_ok(StatusCode::OK, &cfg_body, StatusCode::OK);
    let rev_dis = env.get_plugin_rev("cm.checkin").await;
    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/cm.checkin/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_dis}\"")),
                ("idempotency-key", "chk-cfg-site-a"),
            ],
            Some(json!({
                "schema_revision": cfg["schema_revision"],
                "values": { "site_id": site_a.to_string() },
                "secrets": {}
            })),
        ))
        .await;
    assert_ok(st_put, &b_put, StatusCode::OK);
    let rev_en = env.get_plugin_rev("cm.checkin").await;
    env.enable_plugin("cm.checkin", rev_en, "chk-en-e").await;

    replace_plugin_grants(
        env.state.store().as_ref(),
        "cm.checkin",
        &[PluginGrant {
            capability: "command:site.check-in".into(),
            scope: PluginGrantScope::Site(site_a),
            sensitive: false,
        }],
    );

    let (_, _, t_body) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/cm.checkin/tasks",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let def_rev = t_body["data"][0]["definition_revision"].as_i64().unwrap();
    let (st_up, _, b_up) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/tasks/check-in/update",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{def_rev}\"")),
                ("idempotency-key", "chk-up-e"),
            ],
            Some(json!({ "enabled": true, "schedule": { "interval_secs": 3600 }, "config": {} })),
        ))
        .await;
    assert_ok(st_up, &b_up, StatusCode::ACCEPTED);
    def_rev + 1
}

async fn assert_checkin_config_and_grants(env: &HttpEnv, site_a: SiteId, site_b: SiteId) {
    let next_def_rev = configure_checkin_site_a_and_enable_task(env, site_a).await;

    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/tasks/check-in/run",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{next_def_rev}\"")),
                ("idempotency-key", "chk-run-a"),
            ],
            Some(json!({ "input": {} })),
        ))
        .await;
    assert_ok(st_run, &b_run, StatusCode::ACCEPTED);
    let (st_run_b, _, b_run_b) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/tasks/check-in/run",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{next_def_rev}\"")),
                ("idempotency-key", "chk-run-b"),
            ],
            Some(json!({ "input": { "site_id": site_b.to_string() } })),
        ))
        .await;
    assert_ok(st_run_b, &b_run_b, StatusCode::ACCEPTED);
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
    }

    let list_rc = |sid: SiteId| {
        env.state
            .store()
            .list_site_maintenance_receipts(
                &SiteMaintenanceReceiptFilter {
                    site_id: Some(sid),
                    action: None,
                    state: None,
                },
                None,
                Some(10),
            )
            .unwrap()
            .items
    };
    let rev = env.get_plugin_rev("cm.checkin").await;
    let (st_cmd_a, _, b_cmd_a) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "chk-cmd-a"),
            ],
            Some(json!({
                "kind": "site.check-in",
                "payload": { "site_id": site_a.to_string() }
            })),
        ))
        .await;
    let op_a = assert_ok(st_cmd_a, &b_cmd_a, StatusCode::ACCEPTED);
    let done_a = poll_op(env, op_a["operation_id"].as_str().unwrap()).await;
    assert_eq!(done_a["state"], "succeeded", "done_a={done_a}");
    assert_eq!(
        list_rc(site_a).len(),
        1,
        "granted site_a must succeed check-in"
    );
    assert!(list_rc(site_b).is_empty(), "site_b must NOT be checked in");

    let rev_b = env.get_plugin_rev("cm.checkin").await;
    let (st_cmd_b, _, b_cmd_b) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.checkin/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev_b}\"")),
                ("idempotency-key", "chk-cmd-b"),
            ],
            Some(json!({
                "kind": "site.check-in",
                "payload": { "site_id": site_b.to_string() }
            })),
        ))
        .await;
    assert_err(
        st_cmd_b,
        &b_cmd_b,
        StatusCode::FORBIDDEN,
        "plugin.forbidden",
    );
    assert!(
        list_rc(site_b).is_empty(),
        "unauthorized site_b must be blocked"
    );
}

fn grant_boost_scoped(store: &Store, idx_a: SiteId, dl_id: DownloaderId) {
    let g = |cap: &str, scope: PluginGrantScope| PluginGrant {
        capability: cap.into(),
        scope,
        sensitive: false,
    };
    replace_plugin_grants(
        store,
        "cm.boost",
        &[
            g("kv", PluginGrantScope::Global),
            g("read:boost", PluginGrantScope::Global),
            g("read:downloads", PluginGrantScope::Global),
            g("command:boost.run", PluginGrantScope::Global),
            g("read:sites", PluginGrantScope::Site(idx_a)),
            g("command:indexer.search", PluginGrantScope::Site(idx_a)),
            g(
                "command:downloader.add",
                PluginGrantScope::Object {
                    kind: "downloader".into(),
                    id: dl_id.to_string(),
                },
            ),
        ],
    );
}

async fn assert_boost_grants_and_budget_cap(
    env: &HttpEnv,
    insert_site: &dyn Fn(&str) -> SiteId,
    seen_sites: &Arc<StdMutex<Vec<SiteId>>>,
) {
    let (idx_a, idx_b) = (insert_site("Boost-Idx-A"), insert_site("Boost-Idx-B"));
    let dl_id = DownloaderId::new();
    env.state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "BoostCapDL".into(),
            kind: "qbittorrent".into(),
            url: "http://127.0.0.1:9".into(),
            username: None,
            password: None,
            api_key: None,
            category: None,
            path_maps: vec![],
            is_default: true,
            enabled: true,
        })
        .unwrap();

    save_plugin_config_direct(&env.state, "cm.boost", json!({ "dry_run": false }));
    grant_boost_scoped(env.state.store().as_ref(), idx_a, dl_id);

    let rev = env.get_plugin_rev("cm.boost").await;
    let (st_run, _, b_run) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "boost-cap-run"),
            ],
            Some(json!({
                "kind": "boost.run",
                "payload": { "downloader_id": dl_id.to_string(), "budget_bytes": 4096, "dry_run": false }
            })),
        ))
        .await;
    let run_op = assert_ok(st_run, &b_run, StatusCode::ACCEPTED);
    let done = poll_op(env, run_op["operation_id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "succeeded", "done={done}");
    assert_eq!(
        done["result"]["added"], 0,
        "8192B torrent must be rejected by 4096B budget_cap: {done}"
    );
    assert_eq!(done["result"]["skipped_quota"], true);
    assert_eq!(
        seen_sites.lock().unwrap().clone(),
        vec![idx_a],
        "ungranted idx_b must never be searched"
    );

    let rev2 = env.get_plugin_rev("cm.boost").await;
    let (st_b, _, b_b) = env
        .send(req_json(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev2}\"")),
                ("idempotency-key", "boost-unauth-site"),
            ],
            Some(json!({
                "kind": "boost.run",
                "payload": { "sites": [idx_b.to_string()], "downloader_id": dl_id.to_string(), "dry_run": true }
            })),
        ))
        .await;
    let op_b = assert_ok(st_b, &b_b, StatusCode::ACCEPTED);
    let done_b = poll_op(env, op_b["operation_id"].as_str().unwrap()).await;
    assert_ne!(
        done_b["state"], "succeeded",
        "ungranted site must fail: {done_b}"
    );
}
