use domain::{MaintenanceVia, Site, SiteId};
use indexer::{
    AuthType, FetchRequest, IndexerError, MaintenanceAction as IdxMaintenanceAction,
    MaintenanceDescriptor, ProviderAvailability, ProviderCapability, ProviderDescriptor,
    ProviderSource, SiteFetch,
};
use plugin_api::{
    ApiError, ConfigView, ErrorCode, Header, HttpRequest, HttpResponse, HttpTarget, PageRequest,
    SitePage, SiteView, StatsView,
};
use plugin_host::{HostPorts, Invocation, KvValue};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

pub const POLICY_WASM: &[u8] = include_bytes!("../fixtures/policy.wasm");
pub const TASK_WASM: &[u8] = include_bytes!("../fixtures/task_fixture.wasm");
pub const INDEXER_WASM: &[u8] = include_bytes!("../fixtures/indexer_fixture.wasm");
pub const MAINTENANCE_WASM: &[u8] = include_bytes!("../fixtures/maintenance_fixture.wasm");

pub fn encode_import_component(import_name: &str, export_name: &str) -> Vec<u8> {
    use wasm_encoder::{
        Component, ComponentExportKind, ComponentTypeRef, ComponentTypeSection, InstanceType,
    };
    let mut comp = Component::new();
    let mut types = ComponentTypeSection::new();
    let instance_ty = InstanceType::new();
    types.instance(&instance_ty);
    comp.section(&types);
    let mut imports = wasm_encoder::ComponentImportSection::new();
    imports.import(import_name, ComponentTypeRef::Instance(0));
    comp.section(&imports);
    let mut exports = wasm_encoder::ComponentExportSection::new();
    exports.export(export_name, ComponentExportKind::Instance, 0, None);
    comp.section(&exports);
    comp.finish()
}

pub type HttpCallRecord = (Option<String>, Option<String>, HttpRequest);

pub struct FakeSitePorts {
    pub grants: HashSet<String>,
    pub http_calls: Mutex<Vec<HttpCallRecord>>,
    pub observed_provider_ids: Mutex<Vec<Option<String>>>,
    pub kv: Mutex<HashMap<(String, String), Vec<u8>>>,
    pub epochs: Mutex<HashMap<String, String>>,
    pub inject_set_cookie: bool,
    pub sites: Mutex<HashMap<String, SiteView>>,
}

impl FakeSitePorts {
    pub fn new(grants: &[&str]) -> Self {
        let mut sites = HashMap::new();
        sites.insert(
            "site-a".into(),
            SiteView {
                handle: "site-a".into(),
                id: "site-a".into(),
                name: "Site Alpha".into(),
                enabled: true,
                profile_ref: "plugin:org.example.site/fixture".into(),
                capabilities: vec!["search".into()],
                maintenance_policy: "http".into(),
            },
        );
        sites.insert(
            "site-b".into(),
            SiteView {
                handle: "site-b".into(),
                id: "site-b".into(),
                name: "Site Beta".into(),
                enabled: true,
                profile_ref: "plugin:org.example.site/fixture".into(),
                capabilities: vec!["search".into()],
                maintenance_policy: "browser".into(),
            },
        );
        let mut epochs = HashMap::new();
        epochs.insert("site-a".into(), "epoch-a-1".into());
        epochs.insert("site-b".into(), "epoch-b-1".into());
        Self {
            grants: grants.iter().map(|s| s.to_string()).collect(),
            http_calls: Mutex::new(Vec::new()),
            observed_provider_ids: Mutex::new(Vec::new()),
            kv: Mutex::new(HashMap::new()),
            epochs: Mutex::new(epochs),
            inject_set_cookie: false,
            sites: Mutex::new(sites),
        }
    }

    fn scope_key(inv: &Invocation) -> String {
        format!(
            "{}:{}",
            inv.site_id.as_deref().unwrap_or("global"),
            inv.credential_epoch.as_deref().unwrap_or("none")
        )
    }
}

impl HostPorts for FakeSitePorts {
    fn authorized(&self, inv: &Invocation, cap: &str) -> bool {
        self.observed_provider_ids
            .lock()
            .unwrap()
            .push(inv.provider_id.clone());
        self.grants.contains(cap)
    }

    fn config_get(&self, _inv: &Invocation) -> Result<ConfigView, ApiError> {
        Ok(ConfigView {
            revision: 1,
            validated_json: "{}".into(),
        })
    }

    fn kv_get(&self, inv: &Invocation, key: &str) -> Result<Option<KvValue>, ApiError> {
        let map = self.kv.lock().unwrap();
        Ok(map
            .get(&(Self::scope_key(inv), key.to_string()))
            .map(|v| KvValue {
                value: v.clone(),
                etag: "1".into(),
            }))
    }

    fn kv_put(
        &self,
        inv: &Invocation,
        key: &str,
        value: &[u8],
        _if_match: Option<&str>,
    ) -> Result<String, ApiError> {
        self.kv
            .lock()
            .unwrap()
            .insert((Self::scope_key(inv), key.to_string()), value.to_vec());
        Ok("2".into())
    }

    fn http_fetch(&self, inv: &Invocation, req: HttpRequest) -> Result<HttpResponse, ApiError> {
        self.http_calls.lock().unwrap().push((
            inv.site_id.clone(),
            inv.credential_epoch.clone(),
            req.clone(),
        ));
        let suffix = match &req.target {
            HttpTarget::PublicUrl(u) => u.rsplit('/').next().unwrap_or("").to_string(),
            HttpTarget::SiteRelative(t) => t.path.clone(),
        };
        let mut headers = vec![Header {
            name: "Content-Type".into(),
            value: "text/plain".into(),
        }];
        if self.inject_set_cookie {
            headers.push(Header {
                name: "Set-Cookie".into(),
                value: "secret=leaked; HttpOnly".into(),
            });
            headers.push(Header {
                name: "Authorization".into(),
                value: "Bearer leaked".into(),
            });
        }
        Ok(HttpResponse {
            status: 200,
            headers,
            body: format!("ok-{suffix}").into_bytes(),
            truncated: false,
            receipt: Some("rcpt-1".into()),
        })
    }

    fn site_list(&self, _inv: &Invocation, _page: PageRequest) -> Result<SitePage, ApiError> {
        let items = self.sites.lock().unwrap().values().cloned().collect();
        Ok(SitePage {
            items,
            next_cursor: None,
        })
    }

    fn site_get(&self, _inv: &Invocation, handle: &str) -> Result<SiteView, ApiError> {
        self.sites
            .lock()
            .unwrap()
            .get(handle)
            .cloned()
            .ok_or_else(|| ApiError {
                code: ErrorCode::NotFound,
                message: "site missing".into(),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            })
    }

    fn site_stats(&self, _inv: &Invocation, _handle: &str) -> Result<StatsView, ApiError> {
        Ok(StatsView {
            uploaded: Some(9_999),
            downloaded: Some(1_111),
        })
    }

    fn site_credential_epoch(&self, site_id: &str) -> Result<Option<String>, ApiError> {
        Ok(self.epochs.lock().unwrap().get(site_id).cloned())
    }
}

pub struct NoopFetch;
impl indexer::Fetcher for NoopFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

pub struct NoopSiteFetch;
impl SiteFetch for NoopSiteFetch {
    fn fetch(&self, _: &Site, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

pub fn sample_site() -> Site {
    Site {
        id: SiteId::new(),
        name: "Alpha".into(),
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

pub fn narrow_indexer_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "plugin:org.example.site/fixture".into(),
        name: "Fixture Indexer".into(),
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
            ProviderCapability::ResolveDownload,
        ],
        availability: ProviderAvailability::Enabled,
        hidden_from_catalog: false,
    }
}

pub fn full_indexer_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        capabilities: vec![
            ProviderCapability::Search,
            ProviderCapability::Rss,
            ProviderCapability::Detail,
            ProviderCapability::ResolveDownload,
            ProviderCapability::UserStats,
            ProviderCapability::Categories,
        ],
        ..narrow_indexer_descriptor()
    }
}

pub fn sample_maintenance_descriptor() -> MaintenanceDescriptor {
    MaintenanceDescriptor {
        id: "plugin:org.example.site/checkin".into(),
        source: ProviderSource::PluginWasm {
            plugin_id: "org.example.site".into(),
            component_id: "maint".into(),
            version: "1.0.0".into(),
            plugin_generation: 1,
        },
        actions: vec![IdxMaintenanceAction::CheckIn],
        availability: ProviderAvailability::Enabled,
    }
}
