#[path = "management/common.rs"]
mod common;

use std::sync::{Arc, Mutex};

use api::{ApiState, router};
use axum::http::StatusCode;
use domain::{Site, SiteId, Torrent};
use downloader::MemoryDownloader;
use indexer::{
    AuthType, FetchRequest, Fetcher, IndexerError, IndexerProvider, ProviderAvailability,
    ProviderCallContext, ProviderCapability, ProviderDescriptor, ProviderSource, RegistryCandidate,
    ResolvedDownload, SearchQuery,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const PLUGIN_API_YAML: &str = r#"
id: custom-api
framework: api
base_url: https://custom.example
search:
  path: /api/search
  query_param: keyword
"#;

const PLUGIN_NEXUS_YAML: &str = r#"
id: alpha
framework: nexusphp
base_url: https://alpha.example
search:
  path: /v1/torrents.php
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

struct DisabledStub(ProviderDescriptor);

impl IndexerProvider for DisabledStub {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.0
    }

    fn search(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
        _query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(Vec::new())
    }

    fn rss(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
        _cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(Vec::new())
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

#[tokio::test]
async fn sites_catalog_and_create_use_snapshot_descriptors_without_hardcoding_mteam() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.api", "1.0.0", 1, PLUGIN_API_YAML)
        .unwrap()
        .add_indexer_provider(Arc::new(DisabledStub(ProviderDescriptor {
            id: "plugin:org.example.off/idx".into(),
            name: "Disabled Provider".into(),
            kind: "wasm".into(),
            source: ProviderSource::PluginWasm {
                plugin_id: "org.example.off".into(),
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
        })))
        .unwrap()
        .build()
        .unwrap();

    let state = ApiState::new(
        api::Store::open(tmp.path().join("data")).unwrap(),
        "secret".into(),
        snapshot,
        Arc::new(common::Fixtures {
            requests: Default::default(),
            bodies: Default::default(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("dl"))),
    )
    .unwrap();
    let app = router(state);

    let catalog_resp = app
        .clone()
        .oneshot(common::request(
            "GET",
            "/api/v1/sites/catalog",
            Some("secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(catalog_resp.status(), StatusCode::OK);
    let catalog = common::json_body(catalog_resp).await;
    let items = catalog["data"].as_array().unwrap();
    let custom = items
        .iter()
        .find(|item| item["profile_id"] == "plugin:org.example.api/custom-api")
        .expect("plugin API profile should appear in catalog");
    assert_eq!(custom["auth_types"], json!(["apikey"]));
    assert_eq!(custom["source"], json!("plugin-yaml"));
    assert_eq!(custom["kind"], json!("custom-api"));
    assert_eq!(custom["availability"]["status"], json!("enabled"));

    let disabled = items
        .iter()
        .find(|item| item["profile_id"] == "plugin:org.example.off/idx")
        .unwrap();
    assert_eq!(disabled["availability"]["status"], json!("disabled"));

    // Creating a site referencing a disabled provider is rejected.
    let bad_create = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("secret"),
            json!({
                "name": "OffSite",
                "url": "https://off.example",
                "profile_id": "plugin:org.example.off/idx",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(bad_create.status(), StatusCode::BAD_REQUEST);
    let bad_body = common::json_body(bad_create).await;
    assert_eq!(bad_body["error"]["code"], "site.provider_disabled");

    // Creating a site referencing an enabled plugin YAML profile succeeds using profile base_url.
    let ok_create = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/sites",
            Some("secret"),
            json!({
                "name": "CustomApi",
                "url": "",
                "profile_id": "plugin:org.example.api/custom-api",
                "api_key": "k-123",
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(ok_create.status(), StatusCode::CREATED);
    let ok_body = common::json_body(ok_create).await;
    assert_eq!(ok_body["data"]["url"], "https://custom.example");
}

#[tokio::test]
async fn http_search_fanout_captures_single_snapshot_across_spawned_sites() {
    struct SwappingFetcher {
        state_slot: Mutex<Option<ApiState>>,
        urls: Mutex<Vec<String>>,
    }
    impl Fetcher for SwappingFetcher {
        fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
            self.urls.lock().unwrap().push(req.url.clone());
            if let Some(state) = self.state_slot.lock().unwrap().take() {
                let next = RegistryCandidate::from_builtin()
                    .unwrap()
                    .add_plugin_yaml(
                        "org.example.pt",
                        "2.0.0",
                        2,
                        &PLUGIN_NEXUS_YAML.replace("/v1/torrents.php", "/v2/torrents.php"),
                    )
                    .unwrap();
                state.indexer().publish(next).unwrap();
            }
            Ok("<html><a href='logout.php'>out</a><table><tr class='torrent'><td><a class='name' title='Hit.2026.1080p'>t</a><a class='download' href='/dl?id=1'>d</a></td></tr></table></html>".into())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(SwappingFetcher {
        state_slot: Mutex::new(None),
        urls: Mutex::new(Vec::new()),
    });
    let snap1 = RegistryCandidate::from_builtin()
        .unwrap()
        .add_plugin_yaml("org.example.pt", "1.0.0", 1, PLUGIN_NEXUS_YAML)
        .unwrap()
        .build()
        .unwrap();
    let state = ApiState::new(
        api::Store::open(tmp.path().join("data")).unwrap(),
        "secret".into(),
        snap1,
        fetcher.clone(),
        Arc::new(MemoryDownloader::new(tmp.path().join("dl"))),
    )
    .unwrap();
    *fetcher.state_slot.lock().unwrap() = Some(state.clone());

    for name in ["AlphaOne", "AlphaTwo"] {
        state
            .store()
            .insert_site(&Site {
                id: SiteId::new(),
                name: name.into(),
                url: "https://alpha.example".into(),
                profile_id: "plugin:org.example.pt/alpha".into(),
                cookie: Some("uid=1".into()),
                api_key: None,
                rss_url: None,
                proxy: None,
                rate_limit_per_minute: None,
                cdp_url: None,
                downloader_id: None,
                enabled: true,
                maintenance_via: domain::MaintenanceVia::Http,
                browser_search: false,
                timezone: "UTC".into(),
            })
            .unwrap();
    }

    let app = router(state.clone());
    let resp = app
        .oneshot(common::request(
            "GET",
            "/api/v1/search/torrents?keyword=hit",
            Some("secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = common::json_body(resp).await;
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 2);

    let urls = fetcher.urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 2);
    assert!(
        urls.iter()
            .all(|u| u.contains("/v1/torrents.php") && !u.contains("/v2/torrents.php")),
        "fanout must capture one snapshot for all spawned sites: {urls:?}"
    );
    assert_eq!(state.indexer().snapshot().generation(), 2);
}
