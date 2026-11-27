use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use domain::{Site, SiteId, Torrent};
use parking_lot::Mutex;

use crate::IndexerError;
use crate::fetch::FetchRequest;
use crate::profile::Profile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuthType {
    Cookie,
    ApiKey,
    Credential,
    None,
}

impl AuthType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cookie => "cookie",
            Self::ApiKey => "apikey",
            Self::Credential => "credential",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderCapability {
    Search,
    Rss,
    Detail,
    ResolveDownload,
    UserStats,
    Categories,
}

impl ProviderCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Rss => "rss",
            Self::Detail => "detail",
            Self::ResolveDownload => "resolve-download",
            Self::UserStats => "user-stats",
            Self::Categories => "categories",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MaintenanceAction {
    VerifySession,
    CheckIn,
}

impl MaintenanceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifySession => "verify-session",
            Self::CheckIn => "check-in",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceState {
    Completed,
    AlreadyCompleted,
    AuthExpired,
    Challenge,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceOutcome {
    pub state: MaintenanceState,
    pub evidence: String,
    pub session_capture: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSource {
    Builtin,
    Overlay {
        path: String,
    },
    PluginYaml {
        plugin_id: String,
        version: String,
        plugin_generation: u64,
    },
    PluginWasm {
        plugin_id: String,
        component_id: String,
        version: String,
        plugin_generation: u64,
    },
}

impl ProviderSource {
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Overlay { .. } => "overlay",
            Self::PluginYaml { .. } => "plugin-yaml",
            Self::PluginWasm { .. } => "plugin-wasm",
        }
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Builtin)
    }

    pub fn is_plugin(&self) -> bool {
        matches!(self, Self::PluginYaml { .. } | Self::PluginWasm { .. })
    }

    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::PluginYaml { plugin_id, .. } | Self::PluginWasm { plugin_id, .. } => {
                Some(plugin_id.as_str())
            }
            Self::Builtin | Self::Overlay { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderAvailability {
    Enabled,
    Disabled { reason: String },
}

impl ProviderAvailability {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub source: ProviderSource,
    pub auth_types: Vec<AuthType>,
    pub capabilities: Vec<ProviderCapability>,
    pub availability: ProviderAvailability,
    pub hidden_from_catalog: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceDescriptor {
    pub id: String,
    pub source: ProviderSource,
    pub actions: Vec<MaintenanceAction>,
    pub availability: ProviderAvailability,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchQuery<'a> {
    pub keyword: &'a str,
    pub page: u32,
    pub categories: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedDownload {
    Url(String),
    Request(FetchRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserStats {
    pub uploaded: Option<u64>,
    pub downloaded: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCategory {
    pub id: String,
    pub name: String,
}

pub trait SiteFetch: Send + Sync {
    fn fetch(&self, site: &Site, request: &FetchRequest) -> Result<String, IndexerError>;
}

pub struct ProviderCallContext<'a> {
    pub generation: u64,
    pub fetcher: &'a dyn SiteFetch,
}

pub struct MaintenanceCallContext<'a> {
    pub generation: u64,
    pub fetcher: &'a dyn SiteFetch,
}

#[derive(Debug, Default)]
pub struct SiteRateLimiter {
    next_request: Mutex<HashMap<SiteId, Instant>>,
}

impl SiteRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves the site's next allowed request slot under a short Mutex lock
    /// and sleeps outside the lock.
    pub fn wait_if_needed(&self, site: &Site) {
        let Some(limit) = site.rate_limit_per_minute.filter(|limit| *limit > 0) else {
            return;
        };
        let interval = Duration::from_secs_f64(60.0 / f64::from(limit));
        let delay = {
            let mut next_request = self.next_request.lock();
            let now = Instant::now();
            let scheduled = next_request.get(&site.id).copied().unwrap_or(now).max(now);
            next_request.insert(site.id, scheduled + interval);
            scheduled.saturating_duration_since(now)
        };
        if !delay.is_zero() {
            thread::sleep(delay);
        }
    }
}

pub trait IndexerProvider: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn profile(&self) -> Option<&Arc<Profile>> {
        None
    }

    fn search(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError>;

    fn rss(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError>;

    fn resolve_download(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError>;

    fn detail(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
        _torrent_ref: &str,
    ) -> Result<Torrent, IndexerError> {
        Err(IndexerError::UnsupportedCapability {
            provider_id: self.descriptor().id.clone(),
            capability: "detail".into(),
        })
    }

    fn user_stats(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
    ) -> Result<UserStats, IndexerError> {
        Err(IndexerError::UnsupportedCapability {
            provider_id: self.descriptor().id.clone(),
            capability: "user-stats".into(),
        })
    }

    fn categories(
        &self,
        _ctx: &ProviderCallContext<'_>,
        _site: &Site,
    ) -> Result<Vec<ProviderCategory>, IndexerError> {
        Err(IndexerError::UnsupportedCapability {
            provider_id: self.descriptor().id.clone(),
            capability: "categories".into(),
        })
    }
}

pub trait MaintenanceProvider: Send + Sync {
    fn descriptor(&self) -> &MaintenanceDescriptor;

    fn run(
        &self,
        ctx: &MaintenanceCallContext<'_>,
        site: &Site,
        action: MaintenanceAction,
        job_id: &str,
    ) -> Result<MaintenanceOutcome, IndexerError>;
}
