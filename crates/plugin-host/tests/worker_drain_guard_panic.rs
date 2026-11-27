use domain::{MaintenanceVia, Site, SiteId};
use indexer::{
    AuthType, FetchRequest, IndexerError, IndexerProvider, MaintenanceAction as IdxMaintAction,
    MaintenanceCallContext, MaintenanceDescriptor, MaintenanceProvider, ProviderAvailability,
    ProviderCallContext, ProviderCapability, ProviderDescriptor, ProviderSource, ResolvedDownload,
    SearchQuery as IdxSearchQuery, SiteFetch,
};
use plugin_api::{
    ApiError, Candidate, ConfigView, ErrorCode, Failure, GuestRole, Header, HttpRequest,
    HttpResponse, HttpTarget, PageRequest, PluginTrust, SearchAfter, SitePage, SiteView,
};
use plugin_host::{
    CompiledPolicy, HOOK_WORLD, HostPorts, INDEXER_WORLD, Invocation, KvValue, Limits,
    MAINTENANCE_WORLD, PluginHost, PluginRuntime, WasmIndexerConfig, WasmIndexerProvider,
    WasmMaintenanceConfig, WasmMaintenanceProvider,
};
use std::collections::HashSet;
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

const POLICY_WASM: &[u8] = include_bytes!("fixtures/policy.wasm");
const INDEXER_WASM: &[u8] = include_bytes!("fixtures/indexer_fixture.wasm");
const MAINTENANCE_WASM: &[u8] = include_bytes!("fixtures/maintenance_fixture.wasm");

fn candidate(title: &str) -> Candidate {
    Candidate {
        handle: "opaque-1".into(),
        site_id: "site-1".into(),
        provider: "builtin".into(),
        generation: 1,
        title: title.into(),
        size: Some(10),
        seeders: Some(1),
        leechers: None,
        published_at: None,
        free: None,
        hr: None,
        release: None,
        score: None,
        labels: Vec::new(),
        revision: 1,
    }
}

fn batch(title: &str) -> SearchAfter {
    SearchAfter {
        candidates: vec![candidate(title)],
        site_failures: Vec::new(),
    }
}

struct GatePort {
    entered: mpsc::Sender<String>,
    released: (Mutex<bool>, Condvar),
    panic_plugin: Option<String>,
}

impl GatePort {
    fn new(entered: mpsc::Sender<String>, panic_plugin: Option<&str>) -> Self {
        Self {
            entered,
            released: (Mutex::new(false), Condvar::new()),
            panic_plugin: panic_plugin.map(str::to_string),
        }
    }

    fn release(&self) {
        *self.released.0.lock().unwrap() = true;
        self.released.1.notify_all();
    }
}

impl HostPorts for GatePort {
    fn authorized(&self, _: &Invocation, _: &str) -> bool {
        true
    }

    fn kv_get(&self, inv: &Invocation, _: &str) -> Result<Option<KvValue>, ApiError> {
        if self.panic_plugin.as_deref() == Some(inv.plugin_id.as_str()) {
            panic!("simulated HostPorts callback panic");
        }
        let _ = self.entered.send(inv.plugin_id.clone());
        let mut guard = self.released.0.lock().unwrap();
        while !*guard {
            guard = self.released.1.wait(guard).unwrap();
        }
        Ok(None)
    }
}

#[test]
fn wait_plugin_drained_and_active_count_include_queued_work_and_drop_on_queue_full() {
    let (tx, rx) = mpsc::channel();
    let port = Arc::new(GatePort::new(tx, None));
    let runtime = PluginRuntime::new(Limits {
        workers: 1,
        queue: 1,
        ..Limits::default()
    })
    .unwrap();
    let host = runtime.host(port.clone());
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    let (h1, p1) = (host.clone(), policy.clone());
    let blocker = thread::spawn(move || {
        h1.invoke(
            &p1,
            Invocation::new("plugin-blocker", Duration::from_secs(3)),
            batch("kv"),
        )
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "plugin-blocker"
    );

    let (h2, p2) = (host.clone(), policy.clone());
    let queued = thread::spawn(move || {
        h2.invoke(
            &p2,
            Invocation::new("plugin-queued", Duration::from_secs(3)),
            batch("B"),
        )
    });

    let wait_start = Instant::now();
    while host.active_plugin_count("plugin-queued") == 0
        && wait_start.elapsed() < Duration::from_millis(200)
    {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        host.active_plugin_count("plugin-queued"),
        1,
        "queued WorkItem sitting in sync_channel must be counted in active_plugin_count"
    );
    assert!(
        !host.wait_plugin_drained("plugin-queued", Duration::from_millis(30)),
        "wait_plugin_drained must block while WorkItem is queued behind busy worker"
    );

    let overflow = host.invoke(
        &policy,
        Invocation::new("plugin-overflow", Duration::from_millis(100)),
        batch("B"),
    );
    assert_eq!(overflow.failure, Some(Failure::Busy));
    assert_eq!(host.active_plugin_count("plugin-overflow"), 0);

    port.release();
    assert!(host.wait_plugin_drained("plugin-queued", Duration::from_secs(2)));
    assert_eq!(host.active_plugin_count("plugin-queued"), 0);
    assert!(blocker.join().unwrap().failure.is_none());
    assert!(queued.join().unwrap().failure.is_none());
}

#[test]
fn worker_pool_survives_panicking_host_port_callback_and_processes_subsequent_jobs() {
    let (tx, _rx) = mpsc::channel();
    let port = Arc::new(GatePort::new(tx, Some("panicker")));
    port.release();
    let runtime = PluginRuntime::new(Limits {
        workers: 1,
        ..Limits::default()
    })
    .unwrap();
    let host = runtime.host(port);
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();

    let panicked = host.invoke(
        &policy,
        Invocation::new("panicker", Duration::from_secs(1)),
        batch("kv"),
    );
    assert!(
        matches!(panicked.failure, Some(Failure::Trap(_))),
        "expected Trap on panicking callback, got {:?}",
        panicked.failure
    );
    assert_eq!(host.active_plugin_count("panicker"), 0);

    let survivor = host.invoke(
        &policy,
        Invocation::new("survivor", Duration::from_secs(1)),
        batch("kv"),
    );
    assert!(
        survivor.failure.is_none(),
        "sole worker must survive panic and serve subsequent job, got {:?}",
        survivor.failure
    );
}

struct ActiveDropGuard {
    id: String,
    active_ids: Arc<Mutex<HashSet<String>>>,
}

impl Drop for ActiveDropGuard {
    fn drop(&mut self) {
        self.active_ids.lock().unwrap().remove(&self.id);
    }
}

struct GuardSpyPorts {
    grants: HashSet<String>,
    active_ids: Arc<Mutex<HashSet<String>>>,
    roles: Mutex<Vec<GuestRole>>,
    begin_calls: AtomicUsize,
    block_site: SiteId,
    entered_http: mpsc::Sender<()>,
    release_http: (Mutex<bool>, Condvar),
}

impl HostPorts for GuardSpyPorts {
    fn begin_invocation(
        &self,
        inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        self.begin_calls.fetch_add(1, Ordering::SeqCst);
        self.roles.lock().unwrap().push(inv.role);
        let inserted = self.active_ids.lock().unwrap().insert(inv.id().to_string());
        assert!(
            inserted,
            "begin_invocation called more than once for same invocation {}",
            inv.id()
        );
        Ok(Some(Box::new(ActiveDropGuard {
            id: inv.id().to_string(),
            active_ids: self.active_ids.clone(),
        })))
    }

    fn authorized(&self, inv: &Invocation, cap: &str) -> bool {
        self.active_ids.lock().unwrap().contains(inv.id()) && self.grants.contains(cap)
    }

    fn config_get(&self, _: &Invocation) -> Result<ConfigView, ApiError> {
        Ok(ConfigView {
            revision: 1,
            validated_json: "{}".into(),
        })
    }

    fn site_credential_epoch(&self, _: &str) -> Result<Option<String>, ApiError> {
        Ok(Some("ep-1".into()))
    }

    fn kv_get(&self, _: &Invocation, _: &str) -> Result<Option<KvValue>, ApiError> {
        Ok(None)
    }

    fn kv_put(
        &self,
        _: &Invocation,
        _: &str,
        _: &[u8],
        _: Option<&str>,
    ) -> Result<String, ApiError> {
        Ok("1".into())
    }

    fn site_list(&self, inv: &Invocation, _: PageRequest) -> Result<SitePage, ApiError> {
        let sid = inv.site_id.clone().unwrap_or_else(|| "site-1".into());
        Ok(SitePage {
            items: vec![SiteView {
                handle: sid.clone(),
                id: sid,
                name: "Slow Site".into(),
                enabled: true,
                profile_ref: "plugin:org.example.site/fixture".into(),
                capabilities: vec!["search".into()],
                maintenance_policy: "http".into(),
            }],
            next_cursor: None,
        })
    }

    fn site_get(&self, _: &Invocation, handle: &str) -> Result<SiteView, ApiError> {
        Ok(SiteView {
            handle: handle.into(),
            id: handle.into(),
            name: "Slow Site".into(),
            enabled: true,
            profile_ref: "plugin:org.example.site/fixture".into(),
            capabilities: vec!["search".into()],
            maintenance_policy: "http".into(),
        })
    }

    fn http_fetch(&self, inv: &Invocation, _: HttpRequest) -> Result<HttpResponse, ApiError> {
        if inv.site_id.as_deref() == Some(&self.block_site.to_string()) {
            let _ = self.entered_http.send(());
            let mut released = self.release_http.0.lock().unwrap();
            while !*released {
                released = self.release_http.1.wait(released).unwrap();
            }
        }
        Ok(HttpResponse {
            status: 200,
            headers: vec![Header {
                name: "Content-Type".into(),
                value: "text/plain".into(),
            }],
            body: b"ok".to_vec(),
            truncated: false,
            receipt: None,
        })
    }
}

struct UnusedFetch;
impl SiteFetch for UnusedFetch {
    fn fetch(&self, _: &Site, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn make_site(id: SiteId) -> Site {
    Site {
        id,
        name: "Test".into(),
        url: "https://alpha.invalid/".into(),
        profile_id: "plugin:org.example.site/fixture".into(),
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
    }
}

fn sample_indexer_desc() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "plugin:org.example.site/fixture".into(),
        name: "Fixture".into(),
        kind: "pt".into(),
        source: ProviderSource::PluginWasm {
            plugin_id: "org.example.site".into(),
            component_id: "idx".into(),
            version: "1.0.0".into(),
            plugin_generation: 1,
        },
        auth_types: vec![AuthType::Cookie],
        capabilities: vec![
            ProviderCapability::Search,
            ProviderCapability::Rss,
            ProviderCapability::Detail,
            ProviderCapability::ResolveDownload,
            ProviderCapability::UserStats,
            ProviderCapability::Categories,
        ],
        availability: ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    }
}

#[test]
fn worker_owns_begin_invocation_across_timeout_and_preserves_lifecycle_and_resolve_auth() {
    let fast_site = make_site(SiteId::new());
    let slow_site = make_site(SiteId::new());
    let (tx, rx) = mpsc::channel();
    let ports = Arc::new(GuardSpyPorts {
        grants: ["http", "http:site-auth", "read:sites", "kv"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        active_ids: Arc::new(Mutex::new(HashSet::new())),
        roles: Mutex::new(Vec::new()),
        begin_calls: AtomicUsize::new(0),
        block_site: slow_site.id,
        entered_http: tx,
        release_http: (Mutex::new(false), Condvar::new()),
    });
    let runtime = PluginRuntime::new(Limits {
        provider_wall: Duration::from_millis(350),
        ..Limits::default()
    })
    .unwrap();
    let host = runtime.host(ports.clone());
    let compiled_idx = host.load_indexer(INDEXER_WASM, INDEXER_WORLD).unwrap();
    let compiled_maint = host
        .load_maintenance(MAINTENANCE_WASM, MAINTENANCE_WORLD)
        .unwrap();

    let provider = WasmIndexerProvider::new(
        host.clone(),
        compiled_idx,
        WasmIndexerConfig {
            plugin_id: "org.example.site".into(),
            provider_id: "fixture".into(),
            descriptor: sample_indexer_desc(),
            trust: PluginTrust::BuiltIn,
            rate_limiter: None,
        },
    )
    .unwrap();
    assert_eq!(ports.begin_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ports.roles.lock().unwrap()[0], GuestRole::Lifecycle);

    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &UnusedFetch,
    };
    let torrents = provider
        .search(
            &ctx,
            &fast_site,
            &IdxSearchQuery {
                keyword: "dune",
                page: 1,
                categories: &[],
            },
        )
        .unwrap();
    let dl = provider
        .resolve_download(&ctx, &fast_site, &torrents[0])
        .expect("resolve_download must authorize http:site-auth while worker guard is active");
    assert!(matches!(dl, Some(ResolvedDownload::Request(_))));

    let maint = WasmMaintenanceProvider::new(
        host.clone(),
        compiled_maint,
        WasmMaintenanceConfig {
            plugin_id: "org.example.site".into(),
            provider_id: "checkin".into(),
            descriptor: MaintenanceDescriptor {
                id: "plugin:org.example.site/checkin".into(),
                source: sample_indexer_desc().source,
                actions: vec![IdxMaintAction::CheckIn],
                availability: ProviderAvailability::Enabled,
            },
            trust: PluginTrust::BuiltIn,
            rate_limiter: None,
        },
    );
    let m_ctx = MaintenanceCallContext {
        generation: 1,
        fetcher: &UnusedFetch,
    };
    let maint_thread = thread::spawn(move || {
        maint.run(&m_ctx, &slow_site, IdxMaintAction::CheckIn, "http-checkin")
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let timed_out = maint_thread.join().unwrap();
    assert!(matches!(timed_out, Err(IndexerError::UnknownOutcome(_))));
    assert_eq!(
        ports.active_ids.lock().unwrap().len(),
        1,
        "worker must retain begin_invocation guard after caller DeadlineExceeded"
    );

    *ports.release_http.0.lock().unwrap() = true;
    ports.release_http.1.notify_all();
    assert!(host.wait_plugin_drained("org.example.site", Duration::from_secs(2)));
    assert!(ports.active_ids.lock().unwrap().is_empty());
}

#[derive(Default)]
struct FetchManyGuardPanicPorts {
    in_guest_flags: Mutex<Vec<bool>>,
    reentrant_failures: Mutex<Vec<Option<Failure>>>,
    nested_target: OnceLock<(PluginHost, CompiledPolicy)>,
}

impl HostPorts for FetchManyGuardPanicPorts {
    fn http_fetch(&self, _: &Invocation, req: HttpRequest) -> Result<HttpResponse, ApiError> {
        self.in_guest_flags
            .lock()
            .unwrap()
            .push(plugin_api::in_guest_execution());
        if let Some((host, policy)) = self.nested_target.get() {
            let nested = host.invoke(
                policy,
                Invocation::new("nested-from-http", Duration::from_secs(1)),
                batch("B"),
            );
            self.reentrant_failures.lock().unwrap().push(nested.failure);
        }
        if matches!(&req.target, HttpTarget::PublicUrl(u) if u.contains("panic")) {
            panic!("simulated http_fetch worker panic");
        }
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: b"ok".to_vec(),
            truncated: false,
            receipt: None,
        })
    }
}

fn public_get(url: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        target: HttpTarget::PublicUrl(url.into()),
        headers: Vec::new(),
        body: None,
        max_bytes: 1024,
    }
}

#[test]
fn default_http_fetch_many_enters_guest_execution_guard_and_maps_panic_to_unknown_outcome() {
    let ports = Arc::new(FetchManyGuardPanicPorts::default());
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let host = runtime.host(ports.clone());
    let policy = host.load(POLICY_WASM, HOOK_WORLD).unwrap();
    let _ = ports.nested_target.set((host, policy));

    assert!(!plugin_api::in_guest_execution());
    let inv = Invocation::new("org.example.many", Duration::from_secs(2));
    let results = ports.http_fetch_many(
        &inv,
        vec![
            public_get("https://alpha.invalid/1"),
            public_get("https://alpha.invalid/panic"),
            public_get("https://alpha.invalid/2"),
        ],
    );
    assert!(!plugin_api::in_guest_execution());
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].as_ref().unwrap().status, 200);
    assert_eq!(
        results[1].as_ref().unwrap_err().code,
        ErrorCode::UnknownOutcome
    );
    assert_eq!(results[2].as_ref().unwrap().status, 200);
    assert_eq!(
        *ports.in_guest_flags.lock().unwrap(),
        vec![true, true, true]
    );
    assert_eq!(
        *ports.reentrant_failures.lock().unwrap(),
        vec![
            Some(Failure::Reentrant),
            Some(Failure::Reentrant),
            Some(Failure::Reentrant),
        ]
    );
}
