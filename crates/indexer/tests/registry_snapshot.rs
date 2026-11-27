use std::sync::{Arc, Mutex};

use domain::{Site, SiteId, Torrent};
use indexer::{
    AuthType, FetchRequest, Fetcher, Indexer, IndexerError, IndexerProvider, MaintenanceAction,
    MaintenanceCallContext, MaintenanceDescriptor, MaintenanceOutcome, MaintenanceProvider,
    MaintenanceState, ProviderAvailability, ProviderCallContext, ProviderCapability,
    ProviderDescriptor, ProviderSource, RegistryCandidate, RegistryError, RegistrySnapshot,
    ResolvedDownload, SearchQuery, SiteFailureKind, parse_plugin_provider_id,
};

fn sample_site(id: SiteId, profile_id: &str) -> Site {
    Site {
        id,
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1".into()),
        api_key: Some("key-1".into()),
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

const PLUGIN_NEXUS_YAML: &str = r#"
id: alpha
framework: nexusphp
base_url: https://alpha.example
search:
  path: /torrents.php
  query_param: search
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
    attr: title
  enclosure:
    selector: "a.download"
    attr: href
"#;

const PLUGIN_API_NO_DL_YAML: &str = r#"
id: custom-api
framework: api
base_url: https://custom.example
search:
  path: /api/search
  query_param: keyword
"#;

struct StubProvider {
    descriptor: ProviderDescriptor,
    title: String,
}

impl IndexerProvider for StubProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn search(
        &self,
        _ctx: &ProviderCallContext<'_>,
        site: &Site,
        _query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![Torrent {
            site_id: site.id,
            title: self.title.clone(),
            enclosure: format!("https://pt.example/{}", self.title),
            size_bytes: Some(1024),
            seeders: Some(5),
            free: true,
            hr: None,
            imdb_id: None,
            id: Some("1".into()),
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        }])
    }

    fn rss(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
        _cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Err(IndexerError::UnsupportedCapability {
            provider_id: self.descriptor.id.clone(),
            capability: "rss".into(),
        })
    }

    fn resolve_download(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
        _torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

struct DummyMaint(MaintenanceDescriptor);

impl MaintenanceProvider for DummyMaint {
    fn descriptor(&self) -> &MaintenanceDescriptor {
        &self.0
    }

    fn run(
        &self,
        _ctx: &MaintenanceCallContext<'_>,
        _site: &Site,
        _action: MaintenanceAction,
        _job_id: &str,
    ) -> Result<MaintenanceOutcome, IndexerError> {
        Ok(MaintenanceOutcome {
            state: MaintenanceState::Completed,
            evidence: "ok".into(),
            session_capture: None,
        })
    }
}

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch(format!("unexpected fetch {}", req.url)))
    }
}

#[test]
fn plugin_namespace_validation_and_collision_rejection() {
    assert_eq!(
        parse_plugin_provider_id("plugin:org.example.pt/alpha").unwrap(),
        ("org.example.pt", "alpha")
    );
    for bad in [
        "alpha",
        "plugin:bad/alpha",
        "plugin:installed/alpha",
        "plugin:org.example.pt/BAD",
        "plugin:org.example.pt/a/b",
        "plugin:org.example.pt/",
    ] {
        assert!(
            matches!(
                parse_plugin_provider_id(bad),
                Err(RegistryError::InvalidNamespace { .. })
            ),
            "expected InvalidNamespace for {bad}"
        );
    }

    let snap = RegistrySnapshot::builtin().unwrap();
    let indexer = Indexer::new(snap, Arc::new(NoFetch));

    // Publish valid plugin YAML -> generation advances to 2.
    let cand_v2 = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML)
        .unwrap();
    let snap2 = indexer.publish(cand_v2).unwrap();
    assert_eq!(snap2.generation(), 2);
    let entry = snap2.indexer("plugin:org.example.pt/alpha").unwrap();
    assert_eq!(entry.descriptor().source.as_label(), "plugin-yaml");

    // Duplicate plugin YAML in same candidate must reject atomically.
    let dup = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML)
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML);
    assert!(matches!(dup, Err(RegistryError::Collision { .. })));

    // Plugin YAML trying to masquerade as builtin or overlay must fail.
    let builtin_collide = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(StubProvider {
            descriptor: ProviderDescriptor {
                id: "demo".into(),
                name: "evil".into(),
                kind: "wasm".into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: "org.example.pt".into(),
                    component_id: "idx".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![ProviderCapability::Search],
                availability: ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            title: "Evil".into(),
        }));
    assert!(matches!(
        builtin_collide,
        Err(RegistryError::InvalidNamespace { .. })
    ));
}

#[test]
fn overlay_provenance_and_cas_stale_generation_rejection() {
    // Overlay duplicate within same directory rejects; overlay replacing builtin updates provenance.
    let dup_dir = tempfile::tempdir().unwrap();
    std::fs::write(dup_dir.path().join("a.yaml"), PLUGIN_NEXUS_YAML).unwrap();
    std::fs::write(dup_dir.path().join("b.yaml"), PLUGIN_NEXUS_YAML).unwrap();
    let dup_overlay = RegistryCandidate::from_builtin()
        .unwrap()
        .with_overlay_dir(Some(dup_dir.path()));
    assert!(matches!(dup_overlay, Err(RegistryError::Collision { .. })));

    let plugin_prefix_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin_prefix_dir.path().join("bad.yaml"),
        PLUGIN_NEXUS_YAML.replace("id: alpha", "id: plugin:org.example.pt/alpha"),
    )
    .unwrap();
    let bad_overlay_ns = RegistryCandidate::from_builtin()
        .unwrap()
        .with_overlay_dir(Some(plugin_prefix_dir.path()));
    assert!(matches!(
        bad_overlay_ns,
        Err(RegistryError::InvalidNamespace { .. })
    ));

    let valid_overlay_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        valid_overlay_dir.path().join("demo.yaml"),
        PLUGIN_NEXUS_YAML.replace("id: alpha", "id: demo"),
    )
    .unwrap();
    let ok_overlay = RegistryCandidate::from_builtin()
        .unwrap()
        .with_overlay_dir(Some(valid_overlay_dir.path()))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        ok_overlay
            .indexer("demo")
            .unwrap()
            .descriptor()
            .source
            .as_label(),
        "overlay"
    );

    // Stale expected generation CAS publish must fail without altering snapshot 2.
    let snap = RegistrySnapshot::builtin().unwrap();
    let indexer = Indexer::new(snap, Arc::new(NoFetch));
    let cand_v2 = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML)
        .unwrap();
    let snap2 = indexer.publish(cand_v2).unwrap();
    assert_eq!(snap2.generation(), 2);

    let stale_pub = indexer.publish_if_generation(1, RegistryCandidate::from_builtin().unwrap());
    assert!(matches!(
        stale_pub,
        Err(RegistryError::StaleExpectedGeneration {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(indexer.snapshot().generation(), 2);
    assert!(
        indexer
            .snapshot()
            .indexer("plugin:org.example.pt/alpha")
            .is_some()
    );
}

#[test]
fn single_snapshot_captured_across_fanout_and_generation_fences_download_resolution() {
    struct SwappingFetcher {
        indexer: Mutex<Option<Arc<Indexer>>>,
        urls: Mutex<Vec<String>>,
    }
    impl Fetcher for SwappingFetcher {
        fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
            self.urls.lock().unwrap().push(req.url.clone());
            // Mid-search: publish generation 2 modifying demo/alpha!
            if let Some(idx) = self.indexer.lock().unwrap().take() {
                let next = RegistryCandidate::from_builtin()
                    .unwrap()
                    .add_plugin_yaml(
                        "org.example.pt",
                        "2.0.0",
                        2,
                        &PLUGIN_NEXUS_YAML.replace("/torrents.php", "/v2_torrents.php"),
                    )
                    .unwrap();
                idx.publish(next).unwrap();
            }
            Ok(include_str!("fixtures/nexusphp.html").into())
        }
    }

    let fetcher = Arc::new(SwappingFetcher {
        indexer: Mutex::new(None),
        urls: Mutex::new(Vec::new()),
    });
    let cand1 = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML)
        .unwrap()
        .build()
        .unwrap();
    let indexer = Arc::new(Indexer::new(cand1, fetcher.clone()));
    *fetcher.indexer.lock().unwrap() = Some(Arc::clone(&indexer));

    let s1 = sample_site(SiteId::new(), "plugin:org.example.pt/alpha");
    let s2 = sample_site(SiteId::new(), "plugin:org.example.pt/alpha");
    let outcome = indexer.search(&[s1.clone(), s2], "dune");
    assert_eq!(outcome.generation, 1);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 2);

    let seen_urls = fetcher.urls.lock().unwrap().clone();
    assert_eq!(seen_urls.len(), 2);
    assert!(
        seen_urls
            .iter()
            .all(|u| u.contains("/torrents.php") && !u.contains("/v2_torrents.php")),
        "both sites in one search must stay on generation 1: {seen_urls:?}"
    );
    assert_eq!(indexer.snapshot().generation(), 2);

    // Generation fence on resolve_torrent_download_at.
    let stale_err = indexer
        .resolve_torrent_download_at(&s1, &outcome.torrents[0], 1)
        .unwrap_err();
    assert!(
        matches!(
            stale_err,
            IndexerError::StaleGeneration {
                expected: 1,
                actual: 2
            }
        ),
        "got {stale_err:?}"
    );

    let other_site = sample_site(SiteId::new(), "plugin:org.example.pt/alpha");
    let mismatch_err = indexer
        .resolve_torrent_download_at(&other_site, &outcome.torrents[0], 2)
        .unwrap_err();
    assert!(
        matches!(mismatch_err, IndexerError::InvalidInput(_)),
        "got {mismatch_err:?}"
    );
}

#[test]
fn missing_disabled_unsupported_providers_return_explicit_failures_and_no_mteam_fallback() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    struct Recording(Arc<Mutex<Vec<String>>>);
    impl Fetcher for Recording {
        fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
            self.0.lock().unwrap().push(req.url.clone());
            Ok(include_str!("fixtures/mteam.json").into())
        }
    }

    let mut cand = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.api", "1.0.0", 1, PLUGIN_API_NO_DL_YAML)
        .unwrap()
        .add_indexer_provider(Arc::new(StubProvider {
            descriptor: ProviderDescriptor {
                id: "plugin:org.example.disabled/idx".into(),
                name: "Disabled".into(),
                kind: "wasm".into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: "org.example.disabled".into(),
                    component_id: "idx".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![ProviderCapability::Search],
                availability: ProviderAvailability::Disabled {
                    reason: "draining".into(),
                },
                hidden_from_catalog: false,
            },
            title: "Never".into(),
        }))
        .unwrap();
    cand = cand
        .add_maintenance_provider(Arc::new(DummyMaint(MaintenanceDescriptor {
            id: "plugin:org.example.pt/maint".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.example.pt".into(),
                component_id: "maint".into(),
                version: "1.0.0".into(),
                plugin_generation: 1,
            },
            actions: vec![MaintenanceAction::CheckIn],
            availability: ProviderAvailability::Enabled,
        })))
        .unwrap();
    let snap = cand.build().unwrap();
    assert!(snap.maintenance("plugin:org.example.pt/maint").is_some());

    let indexer = Indexer::new(snap, Arc::new(Recording(calls.clone())));

    let missing_site = sample_site(SiteId::new(), "plugin:org.example.missing/nope");
    let disabled_site = sample_site(SiteId::new(), "plugin:org.example.disabled/idx");
    let api_site = sample_site(SiteId::new(), "plugin:org.example.api/custom-api");

    let outcome = indexer.search(&[missing_site.clone(), disabled_site.clone()], "test");
    assert_eq!(outcome.failures.len(), 2);
    assert!(
        outcome
            .failures
            .iter()
            .any(|f| f.site_id == missing_site.id && f.kind == SiteFailureKind::MissingProvider)
    );
    assert!(
        outcome
            .failures
            .iter()
            .any(|f| f.site_id == disabled_site.id && f.kind == SiteFailureKind::ProviderDisabled)
    );

    // RSS on provider without Rss capability returns UnsupportedCapability failure (never silent empty).
    let rss_outcome = indexer.rss(std::slice::from_ref(&api_site));
    assert_eq!(rss_outcome.failures.len(), 1);
    assert_eq!(
        rss_outcome.failures[0].kind,
        SiteFailureKind::UnsupportedCapability
    );

    // Custom Framework::Api without download config must NOT fall back to M-Team's genDlToken.
    let api_search = indexer.search(std::slice::from_ref(&api_site), "dune");
    assert!(api_search.failures.is_empty(), "{:?}", api_search.failures);
    assert_eq!(api_search.torrents.len(), 1);
    let resolved = indexer
        .resolve_torrent_download(&api_site, &api_search.torrents[0])
        .unwrap();
    assert_eq!(resolved, None);
    let recorded = calls.lock().unwrap().clone();
    assert!(
        !recorded.iter().any(|u| u.contains("genDlToken")),
        "custom API profile must never fall back to M-Team genDlToken: {recorded:?}"
    );
}

#[test]
fn plugin_providers_reject_cross_plugin_id_spoofing_in_indexer_and_maintenance() {
    let spoof_indexer_wasm = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(StubProvider {
            descriptor: ProviderDescriptor {
                id: "plugin:org.victim.pt/alpha".into(),
                name: "Spoof".into(),
                kind: "wasm".into(),
                source: ProviderSource::PluginWasm {
                    plugin_id: "org.evil.attacker".into(),
                    component_id: "idx".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![ProviderCapability::Search],
                availability: ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            title: "Spoof".into(),
        }));
    assert!(
        matches!(
            spoof_indexer_wasm,
            Err(RegistryError::InvalidNamespace { .. })
        ),
        "add_indexer_provider must reject mismatched plugin_id"
    );

    let spoof_indexer_yaml = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(StubProvider {
            descriptor: ProviderDescriptor {
                id: "plugin:org.victim.pt/alpha".into(),
                name: "Spoof".into(),
                kind: "nexusphp".into(),
                source: ProviderSource::PluginYaml {
                    plugin_id: "org.evil.attacker".into(),
                    version: "1.0.0".into(),
                    plugin_generation: 1,
                },
                auth_types: vec![AuthType::Cookie],
                capabilities: vec![ProviderCapability::Search],
                availability: ProviderAvailability::Enabled,
                hidden_from_catalog: false,
            },
            title: "Spoof".into(),
        }));
    assert!(
        matches!(
            spoof_indexer_yaml,
            Err(RegistryError::InvalidNamespace { .. })
        ),
        "add_indexer_provider must reject mismatched PluginYaml plugin_id"
    );

    let spoof_maint = RegistryCandidate::from_builtin()
        .unwrap()
        .add_maintenance_provider(Arc::new(DummyMaint(MaintenanceDescriptor {
            id: "plugin:org.victim.pt/maint".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.evil.attacker".into(),
                component_id: "maint".into(),
                version: "1.0.0".into(),
                plugin_generation: 1,
            },
            actions: vec![MaintenanceAction::CheckIn],
            availability: ProviderAvailability::Enabled,
        })));
    assert!(
        matches!(spoof_maint, Err(RegistryError::InvalidNamespace { .. })),
        "add_maintenance_provider must reject mismatched plugin_id"
    );
}
