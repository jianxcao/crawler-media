use crate::support::{
    FakeSitePorts, INDEXER_WASM, MAINTENANCE_WASM, NoopFetch, NoopSiteFetch,
    full_indexer_descriptor, narrow_indexer_descriptor, sample_maintenance_descriptor, sample_site,
};
use indexer::{
    Indexer, IndexerError, IndexerProvider, MaintenanceAction as IdxMaintenanceAction,
    MaintenanceCallContext, MaintenanceProvider, MaintenanceState as IdxMaintenanceState,
    ProviderCallContext, RegistryCandidate, ResolvedDownload, SiteRateLimiter,
};
use plugin_api::PluginTrust;
use plugin_host::{
    Limits, PluginRuntime, WasmIndexerConfig, WasmIndexerProvider, WasmMaintenanceConfig,
    WasmMaintenanceProvider,
};
use std::sync::Arc;

fn build_wasm_indexer(
    runtime: &PluginRuntime,
    ports: Arc<FakeSitePorts>,
) -> Arc<WasmIndexerProvider> {
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();
    Arc::new(
        WasmIndexerProvider::new(
            host,
            compiled_idx,
            WasmIndexerConfig {
                plugin_id: "org.example.site".into(),
                provider_id: "fixture".into(),
                descriptor: full_indexer_descriptor(),
                trust: PluginTrust::BuiltIn,
                rate_limiter: None,
            },
        )
        .unwrap(),
    )
}

fn search_case(provider: &WasmIndexerProvider, site: &domain::Site, kw: &str) -> domain::Torrent {
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };
    let query = indexer::SearchQuery {
        keyword: kw,
        page: 1,
        categories: &[],
    };
    provider
        .search(&ctx, site, &query)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
}

#[test]
fn wasm_providers_integrate_with_indexer_registry_snapshot() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&[
        "http",
        "http:site-auth",
        "read:sites",
        "kv",
    ]));
    let host = runtime.host(ports.clone());
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();
    let compiled_maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();

    let limiter = Arc::new(SiteRateLimiter::new());
    let wasm_idx = Arc::new(
        WasmIndexerProvider::new(
            host.clone(),
            compiled_idx.clone(),
            WasmIndexerConfig {
                plugin_id: "org.example.site".into(),
                provider_id: "fixture".into(),
                descriptor: full_indexer_descriptor(),
                trust: PluginTrust::BuiltIn,
                rate_limiter: Some(limiter.clone()),
            },
        )
        .expect("matching descriptor succeeds"),
    );

    // Provider with mismatched/unmatched provider_id fails validation
    assert!(
        WasmIndexerProvider::new(
            host.clone(),
            compiled_idx,
            WasmIndexerConfig {
                plugin_id: "org.example.site".into(),
                provider_id: "unknown-provider".into(),
                descriptor: narrow_indexer_descriptor(),
                trust: PluginTrust::BuiltIn,
                rate_limiter: None,
            },
        )
        .is_err()
    );

    let wasm_maint = Arc::new(WasmMaintenanceProvider::new(
        host,
        compiled_maint,
        WasmMaintenanceConfig {
            plugin_id: "org.example.site".into(),
            provider_id: "checkin".into(),
            descriptor: sample_maintenance_descriptor(),
            trust: PluginTrust::BuiltIn,
            rate_limiter: Some(limiter),
        },
    ));

    let snap = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(wasm_idx.clone())
        .unwrap()
        .add_maintenance_provider(wasm_maint.clone())
        .unwrap()
        .build()
        .unwrap();

    let idx_engine = Indexer::new(snap, Arc::new(NoopFetch));
    let site = sample_site();
    ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-site-1".into());
    let outcome = idx_engine.search(std::slice::from_ref(&site), "dune");
    assert!(
        outcome.failures.is_empty(),
        "failures: {:?}",
        outcome.failures
    );
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(outcome.torrents[0].title, "hit:dune");

    let resolved = wasm_idx
        .resolve_download(
            &ProviderCallContext {
                generation: 1,
                fetcher: &NoopSiteFetch,
            },
            &site,
            &outcome.torrents[0],
        )
        .unwrap();
    assert!(matches!(resolved, Some(ResolvedDownload::Request(_))));

    let maint_out = wasm_maint
        .run(
            &MaintenanceCallContext {
                generation: 1,
                fetcher: &NoopSiteFetch,
            },
            &site,
            IdxMaintenanceAction::CheckIn,
            "job-1",
        )
        .unwrap();
    assert_eq!(maint_out.state, IdxMaintenanceState::Completed);
}

#[test]
fn resolve_download_enforces_site_auth_epoch_and_auth_slot_scoping() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let site = sample_site();
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };

    // 1. auth_slot without "http:site-auth" grant -> UnsupportedCapability
    let no_auth_ports = Arc::new(FakeSitePorts::new(&["http", "read:sites", "kv"]));
    no_auth_ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-1".into());
    let no_auth_idx = build_wasm_indexer(&runtime, no_auth_ports);
    let torrent = search_case(&no_auth_idx, &site, "dune");
    let err = no_auth_idx
        .resolve_download(&ctx, &site, &torrent)
        .unwrap_err();
    assert!(matches!(
        err,
        IndexerError::UnsupportedCapability { ref capability, .. } if capability == "http:site-auth"
    ));

    // Unauthenticated site-relative download works without http:site-auth or credential_epoch
    let unauth_torrent = search_case(&no_auth_idx, &site, "dl-case:unauth-site-rel");
    let unauth_dl = no_auth_idx
        .resolve_download(&ctx, &site, &unauth_torrent)
        .unwrap()
        .unwrap();
    match unauth_dl {
        ResolvedDownload::Request(req) => {
            assert_eq!(req.url, "https://alpha.invalid/public/1.torrent?id=9");
            assert_eq!(req.cookie, None);
            assert_eq!(req.api_key, None);
        }
        other => panic!("expected Request, got {other:?}"),
    }

    // 2. auth_slot with "http:site-auth" but missing or empty credential_epoch -> InvalidInput
    let no_epoch_ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    let no_epoch_idx = build_wasm_indexer(&runtime, no_epoch_ports.clone());
    let t_no_epoch = search_case(&no_epoch_idx, &site, "dune");
    assert!(matches!(
        no_epoch_idx.resolve_download(&ctx, &site, &t_no_epoch),
        Err(IndexerError::InvalidInput(_))
    ));
    no_epoch_ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "   ".into());
    let t_empty_epoch = search_case(&no_epoch_idx, &site, "dune");
    assert!(matches!(
        no_epoch_idx.resolve_download(&ctx, &site, &t_empty_epoch),
        Err(IndexerError::InvalidInput(_))
    ));

    // 3. auth_slot targeting another site or empty -> InvalidInput
    let auth_ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    auth_ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-1".into());
    let auth_idx = build_wasm_indexer(&runtime, auth_ports);
    for kw in [
        "dl-case:other-site-auth",
        "dl-case:other-site-auth-upper",
        "dl-case:other-site-auth-mixed",
        "dl-case:empty-auth-slot",
    ] {
        let t = search_case(&auth_idx, &site, kw);
        let res = auth_idx.resolve_download(&ctx, &site, &t);
        assert!(
            matches!(res, Err(IndexerError::InvalidInput(_))),
            "expected InvalidInput for {kw}, got {res:?}"
        );
    }
}

#[test]
fn resolve_download_percent_encodes_query_and_rejects_bad_headers_paths_and_schemes() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let site = sample_site();
    let ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-1".into());
    let idx = build_wasm_indexer(&runtime, ports);
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };

    // Query percent-encoding preserves id=9 and encodes &, =, #, CR, LF, space
    let t_query = search_case(&idx, &site, "dl-case:query-escape");
    match idx
        .resolve_download(&ctx, &site, &t_query)
        .unwrap()
        .unwrap()
    {
        ResolvedDownload::Request(req) => {
            assert_eq!(
                req.url,
                "https://alpha.invalid/download.php?id=9&token=1%26admin%3D1%23frag%0D%0A%20"
            );
            assert_eq!(req.cookie.as_deref(), Some("uid=1"));
        }
        other => panic!("expected Request, got {other:?}"),
    }

    // Valid PublicUrl passes without credentials
    let t_pub = search_case(&idx, &site, "dl-case:public-https");
    assert!(matches!(
        idx.resolve_download(&ctx, &site, &t_pub).unwrap(),
        Some(ResolvedDownload::Url(u)) if u == "https://cdn.example.com/a.torrent"
    ));

    // Forbidden headers, CRLF headers, bad SiteRelative paths, non-http(s)/whitespace PublicUrl rejected
    for kw in [
        "dl-case:forbidden-header",
        "dl-case:set-cookie-header",
        "dl-case:crlf-header",
        "dl-case:bad-path-scheme",
        "dl-case:bad-path-slash",
        "dl-case:bad-path-backslash",
        "dl-case:bad-path-space",
        "dl-case:bad-path-query",
        "dl-case:bad-path-js",
        "dl-case:bad-path-http-colon",
        "dl-case:public-file",
        "dl-case:public-whitespace",
        "dl-case:public-empty-host-port",
        "dl-case:public-backslash",
        "dl-case:public-forbidden-header",
    ] {
        let t = search_case(&idx, &site, kw);
        let res = idx.resolve_download(&ctx, &site, &t);
        assert!(
            matches!(res, Err(IndexerError::InvalidInput(_))),
            "expected InvalidInput for {kw}, got {res:?}"
        );
    }
}

#[test]
fn resolve_download_rejects_public_url_userinfo_invalid_port_and_empty_ipv6() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let site = sample_site();
    let ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-1".into());
    let idx = build_wasm_indexer(&runtime, ports);
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };

    for (kw, expected) in [
        ("dl-case:public-ipv6-ok", "https://[::1]/a.torrent"),
        (
            "dl-case:public-port-ok",
            "https://cdn.example:8443/a.torrent",
        ),
        (
            "dl-case:public-ipv6-port-ok",
            "https://[::1]:8443/a.torrent",
        ),
    ] {
        let t = search_case(&idx, &site, kw);
        assert!(
            matches!(
                idx.resolve_download(&ctx, &site, &t).unwrap(),
                Some(ResolvedDownload::Url(u)) if u == expected
            ),
            "expected Ok(Some(Url({expected}))) for {kw}"
        );
    }

    for kw in [
        "dl-case:public-userinfo",
        "dl-case:public-authority-confusion",
        "dl-case:public-empty-userinfo",
        "dl-case:public-invalid-port",
        "dl-case:public-empty-ipv6",
    ] {
        let t = search_case(&idx, &site, kw);
        let res = idx.resolve_download(&ctx, &site, &t);
        assert!(
            matches!(res, Err(IndexerError::InvalidInput(_))),
            "expected InvalidInput for {kw}, got {res:?}"
        );
    }
}

#[test]
fn wasm_indexer_and_maintenance_propagate_provider_id_on_invocations() {
    assert_eq!(
        plugin_host::Invocation::new("org.example.site", std::time::Duration::from_secs(1))
            .provider_id,
        None
    );

    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&[
        "http",
        "http:site-auth",
        "read:sites",
        "kv",
    ]));
    let site = sample_site();
    ports
        .epochs
        .lock()
        .unwrap()
        .insert(site.id.to_string(), "epoch-1".into());

    let idx = build_wasm_indexer(&runtime, ports.clone());
    ports.observed_provider_ids.lock().unwrap().clear();
    let hit = search_case(&idx, &site, "http-fetch");
    let ctx = ProviderCallContext {
        generation: 1,
        fetcher: &NoopSiteFetch,
    };
    let _ = idx.resolve_download(&ctx, &site, &hit).unwrap();
    let idx_observed = ports.observed_provider_ids.lock().unwrap().clone();
    assert!(!idx_observed.is_empty());
    assert!(
        idx_observed
            .iter()
            .all(|pid| pid.as_deref() == Some("fixture")),
        "expected provider_id=Some(\"fixture\"), got {idx_observed:?}"
    );

    ports.observed_provider_ids.lock().unwrap().clear();
    ports.sites.lock().unwrap().insert(
        site.id.to_string(),
        plugin_api::SiteView {
            handle: site.id.to_string(),
            id: site.id.to_string(),
            name: "Alpha".into(),
            enabled: true,
            profile_ref: site.profile_id.clone(),
            capabilities: vec!["search".into()],
            maintenance_policy: "http".into(),
        },
    );
    let host = runtime.host(ports.clone());
    let compiled_maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();
    let maint = WasmMaintenanceProvider::new(
        host,
        compiled_maint,
        WasmMaintenanceConfig {
            plugin_id: "org.example.site".into(),
            provider_id: "checkin".into(),
            descriptor: sample_maintenance_descriptor(),
            trust: PluginTrust::BuiltIn,
            rate_limiter: None,
        },
    );
    let _ = maint
        .run(
            &MaintenanceCallContext {
                generation: 1,
                fetcher: &NoopSiteFetch,
            },
            &site,
            IdxMaintenanceAction::CheckIn,
            "http-checkin",
        )
        .unwrap();
    let maint_observed = ports.observed_provider_ids.lock().unwrap().clone();
    assert!(!maint_observed.is_empty());
    assert!(
        maint_observed
            .iter()
            .all(|pid| pid.as_deref() == Some("checkin")),
        "expected provider_id=Some(\"checkin\"), got {maint_observed:?}"
    );
}
