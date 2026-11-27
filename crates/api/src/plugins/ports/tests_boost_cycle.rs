use std::sync::Arc;
use std::time::Duration;

use plugin_api::GuestRole;
use plugin_host::{HostPorts, Invocation};
use store::plugins::{PluginGrant, PluginGrantScope};

use super::tests::NoopFetcher;

struct LoopbackTorrentServer {
    addr: std::net::SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl LoopbackTorrentServer {
    fn start() -> Self {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut req_buf = [0u8; 1024];
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                        let n = stream.read(&mut req_buf).unwrap_or(0);
                        let req_str = String::from_utf8_lossy(&req_buf[..n]);
                        let path = req_str
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/t.torrent")
                            .trim_start_matches('/');
                        let bytes = minimal_bencoded_torrent(path, 4096);
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                        let _ = stream.write_all(&bytes);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
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
}

impl Drop for LoopbackTorrentServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn minimal_bencoded_torrent(name: &str, length: usize) -> Vec<u8> {
    let announce = b"http://127.0.0.1:9/announce";
    let mut info = Vec::new();
    info.extend_from_slice(
        format!(
            "d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
            name.len()
        )
        .as_bytes(),
    );
    info.extend_from_slice(&[0u8; 20]);
    info.push(b'e');
    let mut out = format!("d8:announce{}:", announce.len()).into_bytes();
    out.extend_from_slice(announce);
    out.extend_from_slice(b"4:info");
    out.extend_from_slice(&info);
    out.push(b'e');
    out
}

struct StubSearchProvider {
    hits: std::sync::Mutex<Vec<domain::Torrent>>,
    desc: indexer::ProviderDescriptor,
}

impl StubSearchProvider {
    fn new(id: &str, hit: domain::Torrent) -> Self {
        Self {
            hits: std::sync::Mutex::new(vec![hit]),
            desc: indexer::ProviderDescriptor {
                id: id.into(),
                name: format!("Stub:{id}"),
                kind: "builtin".into(),
                source: indexer::ProviderSource::Builtin,
                auth_types: vec![indexer::AuthType::Cookie],
                capabilities: vec![indexer::ProviderCapability::Search],
                availability: indexer::ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
        }
    }
}

impl indexer::IndexerProvider for StubSearchProvider {
    fn descriptor(&self) -> &indexer::ProviderDescriptor {
        &self.desc
    }

    fn search(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
        _: &indexer::SearchQuery<'_>,
    ) -> Result<Vec<domain::Torrent>, indexer::IndexerError> {
        Ok(self.hits.lock().unwrap().clone())
    }

    fn rss(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
        _: Option<&str>,
    ) -> Result<Vec<domain::Torrent>, indexer::IndexerError> {
        Ok(Vec::new())
    }

    fn user_stats(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
    ) -> Result<indexer::UserStats, indexer::IndexerError> {
        Ok(indexer::UserStats {
            uploaded: None,
            downloaded: None,
        })
    }

    fn resolve_download(
        &self,
        _: &indexer::ProviderCallContext<'_>,
        _: &domain::Site,
        _: &domain::Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, indexer::IndexerError> {
        Ok(None)
    }
}

fn setup_policy_enabled_plugin(
    state: &crate::ApiState,
    plugin_id: &str,
    grants: Vec<PluginGrant>,
    priority: i64,
    with_task: bool,
) -> store::plugins::PluginRecord {
    const POLICY_WASM: &[u8] = include_bytes!("../../../../plugin-host/tests/fixtures/policy.wasm");
    let rel = format!("plugins/pkg/{plugin_id}/v1");
    let dir = state.store().data_dir().join(&rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("policy.wasm"), POLICY_WASM).unwrap();
    let mut components = vec![serde_json::json!({
        "id": "policy",
        "entry": "policy.wasm",
        "world": plugin_api::HOOK_WORLD,
        "permissions": ["hooks:download.before", "read:boost"],
        "hooks": [{ "id": "download.before", "priority": priority }]
    })];
    if with_task {
        components.push(serde_json::json!({
            "id": "worker",
            "entry": "policy.wasm",
            "world": plugin_host::TASK_WORLD,
            "permissions": [
                "kv",
                "read:sites",
                "read:downloads",
                "read:boost",
                "command:boost.run",
                "command:indexer.search",
                "command:downloader.add"
            ]
        }));
    }
    let manifest_json = serde_json::json!({
        "manifest_version": 1,
        "id": plugin_id,
        "name": format!("Policy {plugin_id}"),
        "version": "1.0.0",
        "api": "1.0.0",
        "license": "MIT",
        "authors": ["test"],
        "description": "DownloadBefore test hook",
        "config_schema": "config.schema.json",
        "state_schema": 1,
        "components": components,
    })
    .to_string();
    let p = state
        .store()
        .register_plugin_version(
            &store::plugins::PluginVersionMetadata {
                plugin_id: plugin_id.into(),
                package_hash: format!("{plugin_id}:v1"),
                version: "1.0.0".into(),
                manifest_json,
                config_schema_json: "{}".into(),
                state_schema: "1".into(),
                artifact_path: rel,
                signing_key_id: None,
                signature_status: "development".into(),
                ui_metadata_ref: None,
                credential_bound: false,
            },
            0,
        )
        .unwrap();
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
        .finalize_plugin_lifecycle(&store::plugins::PluginLifecycleObservation {
            plugin_id: snap.plugin_id,
            operation_id: snap.active_operation_id.unwrap(),
            revision: snap.revision,
            generation: snap.generation,
            workers_exited: true,
            unknown_effects_reconciled: true,
            validation_succeeded: true,
        })
        .unwrap()
}

fn make_tv_hit(site_id: domain::SiteId, url: &str, title: &str) -> domain::Torrent {
    domain::Torrent {
        site_id,
        id: Some(title.into()),
        title: title.into(),
        enclosure: url.into(),
        size_bytes: Some(4096),
        seeders: Some(10),
        leechers: Some(2),
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: Some("tv".into()),
        free_2x: false,
        free_until: None,
    }
}

struct BoostHookTestEnv {
    _dir: tempfile::TempDir,
    srv: LoopbackTorrentServer,
    dl: Arc<downloader::MemoryDownloader>,
    state: crate::ApiState,
    site_id: domain::SiteId,
    dl_id: domain::DownloaderId,
    provider: Arc<StubSearchProvider>,
    boost_self: store::plugins::PluginRecord,
}

fn setup_boost_hook_env() -> BoostHookTestEnv {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = LoopbackTorrentServer::start();
    let store = Arc::new(crate::Store::open(dir.path().join("data")).unwrap());
    let dl = Arc::new(downloader::MemoryDownloader::new(dir.path().join("dl")));
    let state = crate::ApiState::new_arc(
        store,
        "test-tok".into(),
        indexer::RegistrySnapshot::builtin().unwrap(),
        Arc::new(NoopFetcher),
        dl.clone(),
    )
    .unwrap()
    .with_plugin_http_policy_override(
        net_guard::TargetPolicy::production().with_test_loopback_socket(srv.addr),
    );
    let site_id = domain::SiteId::new();
    let hit_url = format!("http://{}/t1.torrent", srv.addr);
    let provider = Arc::new(StubSearchProvider::new(
        "boost_task_idx",
        make_tv_hit(site_id, &hit_url, "GuestCycleShow.S01E01"),
    ));
    let reg = indexer::RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(provider.clone())
        .unwrap();
    state.indexer.publish(reg).unwrap();
    state
        .store()
        .insert_site(&domain::Site {
            id: site_id,
            name: "LoopbackPT".into(),
            url: format!("http://{}", srv.addr),
            profile_id: "boost_task_idx".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: Some(600),
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    let dl_id = domain::DownloaderId::new();
    state
        .store()
        .insert_downloader(&store::DownloaderRow {
            id: dl_id,
            name: "MemDL".into(),
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
    let boost_grants = [
        "kv",
        "read:sites",
        "read:downloads",
        "read:boost",
        "command:boost.run",
        "command:indexer.search",
        "command:downloader.add",
        "hooks:download.before",
    ]
    .into_iter()
    .map(|cap| PluginGrant {
        capability: cap.into(),
        scope: PluginGrantScope::Global,
        sensitive: false,
    })
    .collect();
    let boost_self =
        setup_policy_enabled_plugin(&state, "org.example.boostself", boost_grants, 10, true);
    let _other_hook = setup_policy_enabled_plugin(
        &state,
        "org.example.otherhook",
        ["hooks:download.before", "read:boost"]
            .into_iter()
            .map(|cap| PluginGrant {
                capability: cap.into(),
                scope: PluginGrantScope::Global,
                sensitive: false,
            })
            .collect(),
        20,
        false,
    );
    BoostHookTestEnv {
        _dir: dir,
        srv,
        dl,
        state,
        site_id,
        dl_id,
        provider,
        boost_self,
    }
}

#[test]
fn boost_run_cycle_runs_off_guest_thread_and_excludes_self_hook() {
    let env = setup_boost_hook_env();
    let ports = env.state.plugins().ports();
    let mut inv = Invocation::new(env.boost_self.plugin_id.clone(), Duration::from_secs(10));
    inv.generation = u64::try_from(env.boost_self.generation).unwrap();
    inv.role = GuestRole::Task;
    let _g = ports.begin_invocation(&inv).unwrap();
    let _ts = ports.register_task_state(inv.id().to_string(), env.state.clone());

    let cfg_json = serde_json::json!({
        "dry_run": false,
        "downloader_id": env.dl_id.to_string(),
        "sites": [env.site_id.to_string()],
        "categories": ["tv"],
        "free_mode": "free",
        "exclude_hr": true,
    })
    .to_string();

    let raw_out = {
        let _guest = plugin_api::GuestExecutionGuard::enter();
        ports.boost_run_cycle(&inv, &cfg_json).unwrap()
    };
    let summary: serde_json::Value = serde_json::from_str(&raw_out).unwrap();
    assert_eq!(
        summary["added"], 1,
        "boost_run_cycle must submit candidate off guest thread: {summary}"
    );
    let submitted = env.dl.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        submitted[0].options.save_path.as_deref(),
        Some("plugins/demo")
    );

    let other_audits = env
        .state
        .store()
        .list_plugin_audit_events(
            "org.example.otherhook",
            Some("download.before"),
            None,
            None,
            Some(20),
        )
        .unwrap();
    assert!(
        !other_audits.items.is_empty(),
        "other plugins' download.before hooks must still run"
    );
    let self_audits = env
        .state
        .store()
        .list_plugin_audit_events(
            "org.example.boostself",
            Some("download.before"),
            None,
            None,
            Some(20),
        )
        .unwrap();
    assert!(
        self_audits.items.is_empty(),
        "self download.before hook must be excluded during boost.run-cycle: {self_audits:?}"
    );

    *env.provider.hits.lock().unwrap() = vec![make_tv_hit(
        env.site_id,
        &format!("http://{}/t2.torrent", env.srv.addr),
        "AdminCycleShow.S01E02",
    )];
    let cfg = crate::plugins::boost::config::BoostConfig::from_layers(&[&serde_json::from_str::<
        serde_json::Value,
    >(&cfg_json)
    .unwrap()])
    .unwrap();
    let admin_summary = crate::plugins::boost::cycle::execute_boost_cycle(
        &env.state,
        &env.boost_self,
        &cfg,
        "admin-cause",
    )
    .unwrap();
    assert_eq!(admin_summary.added, 1);
    let self_audits_after_admin = env
        .state
        .store()
        .list_plugin_audit_events(
            "org.example.boostself",
            Some("download.before"),
            None,
            None,
            Some(20),
        )
        .unwrap();
    assert!(
        !self_audits_after_admin.items.is_empty(),
        "admin execute_boost_cycle must still run boostself's download.before hook"
    );
}
