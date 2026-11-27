use crate::traversal_wat::{INDEXER_TRAVERSAL_WAT, MAINT_TRAVERSAL_WAT};
use domain::{MaintenanceVia, Site, SiteId};
use indexer::{
    AuthType, FetchRequest, IndexerError, IndexerProvider, ProviderAvailability,
    ProviderCallContext, ProviderCapability, ProviderDescriptor, ProviderSource, ResolvedDownload,
    SearchQuery as IdxSearchQuery, SiteFetch,
};
use plugin_api::{
    ApiError, BrowserElement, BrowserFillValue, BrowserPurpose, ErrorCode, Failure, HttpRequest,
    HttpResponse, MaintenanceAction, PluginTrust,
};
use plugin_host::{
    BrowserSessionPort, HostPorts, INDEXER_WORLD, Invocation, Limits, MAINTENANCE_WORLD,
    PluginRuntime, WasmIndexerConfig, WasmIndexerProvider,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct TraversalBrowserSession {
    nav_calls: Arc<AtomicUsize>,
    text_calls: Arc<AtomicUsize>,
}

impl BrowserSessionPort for TraversalBrowserSession {
    fn navigate(&mut self, site_relative_url: &str, _: &str) -> Result<String, ApiError> {
        self.nav_calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("rcpt:{site_relative_url}"))
    }
    fn dom(&mut self, _: u32) -> Result<String, ApiError> {
        Ok("<html/>".into())
    }
    fn query(&mut self, _: &str, _: u32) -> Result<Vec<BrowserElement>, ApiError> {
        Ok(vec![BrowserElement {
            handle: "backend-el-1".into(),
            visible_text: "Row".into(),
        }])
    }
    fn text(&mut self, element: &str, _: u32) -> Result<String, ApiError> {
        assert_eq!(element, "backend-el-1");
        self.text_calls.fetch_add(1, Ordering::SeqCst);
        Ok("Row".into())
    }
    fn attribute(&mut self, _: &str, _: &str) -> Result<Option<String>, ApiError> {
        Ok(None)
    }
    fn click(&mut self, _: &str) -> Result<String, ApiError> {
        Ok("clicked".into())
    }
    fn fill(&mut self, _: &str, _: BrowserFillValue) -> Result<(), ApiError> {
        Ok(())
    }
    fn wait_for(&mut self, _: Option<&str>, _: &str, _: u64) -> Result<String, ApiError> {
        Ok("ready".into())
    }
    fn read_response(&mut self, _: &str, _: u32) -> Result<HttpResponse, ApiError> {
        Err(ApiError {
            code: ErrorCode::Unsupported,
            message: "unused".into(),
            detail: None,
            retry_after_ms: None,
            operation_id: None,
        })
    }
    fn capture_session(&mut self) -> Result<String, ApiError> {
        Ok("cap".into())
    }
    fn close(&mut self) -> Result<(), ApiError> {
        Ok(())
    }
}

struct TraversalPorts {
    http_calls: Mutex<Vec<HttpRequest>>,
    nav_calls: Arc<AtomicUsize>,
    text_calls: Arc<AtomicUsize>,
}

impl TraversalPorts {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            http_calls: Mutex::new(Vec::new()),
            nav_calls: Arc::new(AtomicUsize::new(0)),
            text_calls: Arc::new(AtomicUsize::new(0)),
        })
    }
}

impl HostPorts for TraversalPorts {
    fn authorized(&self, _: &Invocation, cap: &str) -> bool {
        matches!(cap, "http" | "http:site-auth" | "browser:maintenance")
    }

    fn site_credential_epoch(&self, _: &str) -> Result<Option<String>, ApiError> {
        Ok(Some("epoch-1".into()))
    }

    fn http_fetch(&self, _: &Invocation, req: HttpRequest) -> Result<HttpResponse, ApiError> {
        self.http_calls.lock().unwrap().push(req);
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: b"ok".to_vec(),
            truncated: false,
            receipt: None,
        })
    }

    fn browser_open(
        &self,
        _: &Invocation,
        _: BrowserPurpose,
    ) -> Result<Box<dyn BrowserSessionPort>, ApiError> {
        Ok(Box::new(TraversalBrowserSession {
            nav_calls: self.nav_calls.clone(),
            text_calls: self.text_calls.clone(),
        }))
    }
}

struct NoopSiteFetch;
impl SiteFetch for NoopSiteFetch {
    fn fetch(&self, _: &Site, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn sample_site() -> Site {
    Site {
        id: SiteId::new(),
        name: "Alpha".into(),
        url: "https://alpha.invalid/tracker".into(),
        profile_id: "plugin:org.example.trav/trav".into(),
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

#[test]
fn resolve_download_rejects_site_relative_traversal_and_trims_site_auth_slot() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = TraversalPorts::new();
    let host = runtime.host(ports);
    let compiled = host
        .load_indexer(INDEXER_TRAVERSAL_WAT.as_bytes(), INDEXER_WORLD)
        .unwrap();
    let provider = WasmIndexerProvider::new(
        host,
        compiled,
        WasmIndexerConfig {
            plugin_id: "org.example.trav".into(),
            provider_id: "trav".into(),
            descriptor: ProviderDescriptor {
                id: "plugin:org.example.trav/trav".into(),
                name: "trav".into(),
                kind: "trav".into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: "org.example.trav".into(),
                    component_id: "idx".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![
                    ProviderCapability::Search,
                    ProviderCapability::ResolveDownload,
                ],
                availability: ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            trust: PluginTrust::BuiltIn,
            rate_limiter: None,
        },
    )
    .unwrap();

    let site = sample_site();
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };

    for bad_path in [
        "../admin",
        "/tracker/..%2fadmin",
        "%2e%2e/secret",
        "/tracker/%2E%2E/secret",
        "/tracker/.;jsession=1/admin",
        "/tracker/%5cadmin",
    ] {
        let found = provider
            .search(
                &ctx,
                &site,
                &IdxSearchQuery {
                    keyword: bad_path,
                    page: 1,
                    categories: &[],
                },
            )
            .unwrap();
        let err = provider
            .resolve_download(&ctx, &site, &found[0])
            .expect_err(&format!(
                "expected resolve_download rejection for `{bad_path}`"
            ));
        assert!(
            matches!(err, IndexerError::InvalidInput(_)),
            "expected InvalidInput for `{bad_path}`, got {err:?}"
        );
    }

    // Trimmed `site: <same-site-id>` succeeds while `site:   ` fails as InvalidInput.
    let spaced_same = format!("@site:  {}  ", site.id);
    let ok_t = provider
        .search(
            &ctx,
            &site,
            &IdxSearchQuery {
                keyword: &spaced_same,
                page: 1,
                categories: &[],
            },
        )
        .unwrap();
    let dl = provider.resolve_download(&ctx, &site, &ok_t[0]).unwrap();
    assert!(matches!(dl, Some(ResolvedDownload::Request(_))));

    let empty_slot_t = provider
        .search(
            &ctx,
            &site,
            &IdxSearchQuery {
                keyword: "@site:   ",
                page: 1,
                categories: &[],
            },
        )
        .unwrap();
    assert!(matches!(
        provider.resolve_download(&ctx, &site, &empty_slot_t[0]),
        Err(IndexerError::InvalidInput(_))
    ));
}

#[test]
fn http_fetch_and_browser_navigate_reject_traversal_without_incrementing_doc_epoch() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = TraversalPorts::new();
    let host = runtime.host(ports.clone());
    let maint = host
        .load_maintenance(MAINT_TRAVERSAL_WAT.as_bytes(), MAINTENANCE_WORLD)
        .unwrap();

    let mut inv = Invocation::new("org.example.trav", Duration::from_secs(3));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-1".into());
    inv.trust = PluginTrust::BuiltIn;

    for bad_path in [
        "../admin",
        "/tracker/..%2fadmin",
        "%2e%2e/secret",
        "/tracker/.;m=1/admin",
        "/tracker/%5csecret",
    ] {
        // 1. http.fetch SiteRelative rejects traversal before reaching HostPorts::http_fetch
        let http_err = host
            .maintenance_run(
                &maint,
                inv.clone(),
                MaintenanceAction::VerifySession,
                bad_path.into(),
            )
            .unwrap_err();
        assert!(
            matches!(
                http_err,
                Failure::Guest(ApiError {
                    code: ErrorCode::InvalidArgument,
                    ..
                })
            ),
            "http.fetch SiteRelative `{bad_path}` expected InvalidArgument, got {http_err:?}"
        );
        assert!(ports.http_calls.lock().unwrap().is_empty());

        // 2. browser.navigate rejects traversal BEFORE mutating doc_epoch or issued_handles
        ports.nav_calls.store(0, Ordering::SeqCst);
        ports.text_calls.store(0, Ordering::SeqCst);
        let nav_err = host
            .maintenance_run(
                &maint,
                inv.clone(),
                MaintenanceAction::CheckIn,
                bad_path.into(),
            )
            .unwrap_err();
        assert!(
            matches!(
                nav_err,
                Failure::Guest(ApiError {
                    code: ErrorCode::InvalidArgument,
                    ..
                })
            ),
            "browser.navigate `{bad_path}` expected InvalidArgument (not StaleHandle/Conflict), got {nav_err:?}"
        );
        assert_eq!(
            ports.nav_calls.load(Ordering::SeqCst),
            1,
            "second navigate `{bad_path}` must not reach BrowserSessionPort"
        );
        assert_eq!(
            ports.text_calls.load(Ordering::SeqCst),
            1,
            "element handle from prior epoch must remain valid after rejected navigate"
        );
    }
}

#[test]
fn host_indexer_resolve_download_directly_enforces_http_request_sandbox_and_auth_scoping() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = TraversalPorts::new();
    let host = runtime.host(ports);
    let compiled = host
        .load_indexer(INDEXER_TRAVERSAL_WAT.as_bytes(), INDEXER_WORLD)
        .unwrap();

    let mut inv = Invocation::new("org.example.trav", Duration::from_secs(3));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-1".into());
    inv.generation = 1;
    inv.trust = PluginTrust::BuiltIn;

    let resolve_raw = |inv_arg: Invocation, raw_kw: &str| {
        let batch = host
            .indexer_search(
                &compiled,
                inv_arg.clone(),
                "trav",
                plugin_api::SearchQuery {
                    keyword: raw_kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap();
        host.indexer_resolve_download(
            &compiled,
            inv_arg,
            "trav",
            batch.items[0].download_ref.clone(),
        )
    };

    for (raw_kw, expected_code) in [
        ("../admin", ErrorCode::InvalidArgument),
        ("/tracker/..%2fadmin", ErrorCode::InvalidArgument),
        ("@site:site-b", ErrorCode::PermissionDenied),
        ("@site:   ", ErrorCode::InvalidArgument),
        ("#Cookie", ErrorCode::PermissionDenied),
        ("#Bad\nHeader", ErrorCode::InvalidArgument),
        ("!file:///etc/passwd", ErrorCode::InvalidArgument),
        ("$DELETE", ErrorCode::InvalidArgument),
    ] {
        let err = resolve_raw(inv.clone(), raw_kw).unwrap_err();
        assert!(
            matches!(&err, Failure::Guest(ApiError { code, .. }) if *code == expected_code),
            "raw `{raw_kw}` expected {expected_code:?}, got {err:?}"
        );
    }

    let mut inv_no_site = inv.clone();
    inv_no_site.site_id = None;
    let err_no_site = resolve_raw(inv_no_site, "/dl").unwrap_err();
    assert!(matches!(
        err_no_site,
        Failure::Guest(ApiError {
            code: ErrorCode::WrongContext,
            ..
        })
    ));

    let mut inv_no_epoch = inv.clone();
    inv_no_epoch.credential_epoch = None;
    let err_no_epoch = resolve_raw(inv_no_epoch, "@site:site-a").unwrap_err();
    assert!(matches!(
        err_no_epoch,
        Failure::Guest(ApiError {
            code: ErrorCode::WrongContext,
            ..
        })
    ));

    assert!(resolve_raw(inv.clone(), "@site:site-a").is_ok());
    assert!(resolve_raw(inv, "!https://cdn.example/dl.torrent").is_ok());
}

fn build_traversal_indexer(host: plugin_host::PluginHost) -> WasmIndexerProvider {
    let compiled = host
        .load_indexer(INDEXER_TRAVERSAL_WAT.as_bytes(), INDEXER_WORLD)
        .unwrap();
    WasmIndexerProvider::new(
        host,
        compiled,
        WasmIndexerConfig {
            plugin_id: "org.example.trav".into(),
            provider_id: "trav".into(),
            descriptor: ProviderDescriptor {
                id: "plugin:org.example.trav/trav".into(),
                name: "trav".into(),
                kind: "trav".into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: "org.example.trav".into(),
                    component_id: "idx".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![
                    ProviderCapability::Search,
                    ProviderCapability::ResolveDownload,
                ],
                availability: ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            trust: PluginTrust::BuiltIn,
            rate_limiter: None,
        },
    )
    .unwrap()
}

#[test]
fn public_url_rejects_traversal_bad_method_and_isolates_same_origin_credentials() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let provider = build_traversal_indexer(runtime.host(TraversalPorts::new()));
    let mut site = sample_site();
    site.cookie = Some("uid=secret".into());
    site.api_key = Some("api-secret".into());
    site.proxy = Some("http://127.0.0.1:8080".into());
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };

    let resolve_kw = |kw: &str| {
        let found = provider
            .search(
                &ctx,
                &site,
                &IdxSearchQuery {
                    keyword: kw,
                    page: 1,
                    categories: &[],
                },
            )
            .unwrap();
        provider.resolve_download(&ctx, &site, &found[0])
    };

    for bad in [
        "!https://cdn.example/../secret",
        "!https://cdn.example/%2e%2e/secret",
        "!https://[::1]/../secret",
        "^DELETE",
    ] {
        let res = resolve_kw(bad);
        assert!(
            matches!(res, Err(IndexerError::InvalidInput(_))),
            "expected InvalidInput for `{bad}`, got {res:?}"
        );
    }

    for external in ["!https://[::1]/a.torrent", "!https://cdn.example/a.torrent"] {
        let expected = &external[1..];
        assert!(matches!(
            resolve_kw(external).unwrap(),
            Some(ResolvedDownload::Url(ref u)) if u == expected
        ));
    }

    match resolve_kw("^POST").unwrap().unwrap() {
        ResolvedDownload::Request(req) => {
            assert_eq!(req.method, indexer::FetchMethod::PostForm);
            assert_eq!(req.url, "https://cdn.example/dl.torrent");
            assert_eq!(req.body.as_deref(), Some("v=1"));
            assert_eq!(req.cookie, None);
            assert_eq!(req.api_key, None);
            assert_eq!(req.proxy, None);
        }
        other => panic!("expected Request for POST PublicUrl, got {other:?}"),
    }

    for same_origin in [
        "!https://alpha.invalid/public.torrent",
        "!https://ALPHA.INVALID.:443/public.torrent",
    ] {
        match resolve_kw(same_origin).unwrap().unwrap() {
            ResolvedDownload::Request(req) => {
                assert_eq!(req.method, indexer::FetchMethod::Get);
                assert_eq!(req.url, &same_origin[1..]);
                assert_eq!(req.cookie, None);
                assert_eq!(req.api_key, None);
                assert_eq!(req.proxy, None);
            }
            other => panic!("same-origin `{same_origin}` must return Request, got {other:?}"),
        }
    }
}

#[test]
fn content_length_header_is_rejected_in_http_fetch_and_resolve_download() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = TraversalPorts::new();
    let host = runtime.host(ports.clone());
    let maint = host
        .load_maintenance(MAINT_TRAVERSAL_WAT.as_bytes(), MAINTENANCE_WORLD)
        .unwrap();
    let compiled_idx = host
        .load_indexer(INDEXER_TRAVERSAL_WAT.as_bytes(), INDEXER_WORLD)
        .unwrap();
    let provider = build_traversal_indexer(host.clone());

    let mut inv = Invocation::new("org.example.trav", Duration::from_secs(3));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-1".into());
    inv.trust = PluginTrust::BuiltIn;

    for hdr in ["#Content-Length", "#content-length"] {
        let fetch_err = host
            .maintenance_run(
                &maint,
                inv.clone(),
                MaintenanceAction::VerifySession,
                hdr.into(),
            )
            .unwrap_err();
        assert!(
            matches!(
                fetch_err,
                Failure::Guest(ApiError {
                    code: ErrorCode::PermissionDenied,
                    ..
                })
            ),
            "http.fetch `{hdr}` expected PermissionDenied, got {fetch_err:?}"
        );
        assert!(ports.http_calls.lock().unwrap().is_empty());

        let batch = host
            .indexer_search(
                &compiled_idx,
                inv.clone(),
                "trav",
                plugin_api::SearchQuery {
                    keyword: hdr.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap();
        let host_dl_err = host
            .indexer_resolve_download(
                &compiled_idx,
                inv.clone(),
                "trav",
                batch.items[0].download_ref.clone(),
            )
            .unwrap_err();
        assert!(matches!(
            host_dl_err,
            Failure::Guest(ApiError {
                code: ErrorCode::PermissionDenied,
                ..
            })
        ));

        let site = sample_site();
        let ctx = ProviderCallContext {
            generation: 1,
            fetcher: &NoopSiteFetch,
        };
        let found = provider
            .search(
                &ctx,
                &site,
                &IdxSearchQuery {
                    keyword: hdr,
                    page: 1,
                    categories: &[],
                },
            )
            .unwrap();
        assert!(matches!(
            provider.resolve_download(&ctx, &site, &found[0]),
            Err(IndexerError::InvalidInput(_))
        ));
    }
}

#[test]
fn public_url_isolates_percent_encoded_idna_and_ipv4_radix_same_origin_variants() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let provider = build_traversal_indexer(runtime.host(TraversalPorts::new()));
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };
    let resolve_for_site = |site_url: &str, kw: &str| {
        let mut site = sample_site();
        site.url = site_url.into();
        site.cookie = Some("uid=secret".into());
        site.api_key = Some("api-secret".into());
        site.proxy = Some("http://127.0.0.1:8080".into());
        let found = provider
            .search(
                &ctx,
                &site,
                &IdxSearchQuery {
                    keyword: kw,
                    page: 1,
                    categories: &[],
                },
            )
            .unwrap();
        provider.resolve_download(&ctx, &site, &found[0])
    };

    for (site_url, kw) in [
        (
            "https://alpha.invalid",
            "!https://%61lpha.invalid/dl.torrent",
        ),
        (
            "https://alpha.invalid",
            "!https://alpha%2einvalid/dl.torrent",
        ),
        ("https://93.184.216.34", "!https://0x5db8d822/dl.torrent"),
        ("https://93.184.216.34", "!https://1572395042/dl.torrent"),
        (
            "https://münchen.example",
            "!https://xn--mnchen-3ya.example/dl.torrent",
        ),
    ] {
        match resolve_for_site(site_url, kw).unwrap().unwrap() {
            ResolvedDownload::Request(req) => {
                assert_eq!(req.cookie, None, "case ({site_url}, {kw})");
                assert_eq!(req.api_key, None, "case ({site_url}, {kw})");
                assert_eq!(req.proxy, None, "case ({site_url}, {kw})");
            }
            other => {
                panic!("same-origin alias ({site_url}, {kw}) must return Request, got {other:?}")
            }
        }
    }

    assert!(matches!(
        resolve_for_site("https://alpha.invalid", "!http://169.254.169.254/latest"),
        Err(IndexerError::InvalidInput(_))
    ));
}
