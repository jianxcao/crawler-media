mod browser;
pub mod browser_host;
pub mod cdp;
pub mod cdp_page;
mod engine;
mod error;
mod fetch;
mod native_providers;
mod parse;
mod plugin_browser;
mod profile;
mod provider;
mod registry;
mod tz;
mod user_stats;

pub use browser::{
    Browser, BrowserConfig, BrowserRoute, PageSession, RecordingFetcher, RenderedPage,
};
pub use cdp::{CdpCookie, DiscoveredSiteCookie, fetch_cookies_from_cdp, match_cookies_to_profiles};
pub use engine::{Indexer, SearchOutcome, SiteFailure};
pub use error::{IndexerError, RegistryError, SiteFailureKind};
pub use fetch::{FetchMethod, FetchRequest, Fetcher};
pub use native_providers::{GenericApiYamlProvider, MTeamProvider, NexusPhpYamlProvider};
pub use plugin_browser::{
    CdpPluginPage, PluginBrowserError, PluginBrowserErrorKind, PluginBrowserLease,
    PluginBrowserPage, PluginNavResponse,
};
pub use profile::{ApiDialect, CategoryMapping, Framework, Profile, normalize_category};
pub use provider::{
    AuthType, IndexerProvider, MaintenanceAction, MaintenanceCallContext, MaintenanceDescriptor,
    MaintenanceOutcome, MaintenanceProvider, MaintenanceState, ProviderAvailability,
    ProviderCallContext, ProviderCapability, ProviderCategory, ProviderDescriptor, ProviderSource,
    ResolvedDownload, SearchQuery, SiteFetch, SiteRateLimiter, UserStats,
};
pub use registry::{
    IndexerEntry, MaintenanceEntry, RegistryCandidate, RegistrySnapshot, parse_plugin_provider_id,
};
pub use tz::{parse_site_datetime, resolve_offset_secs};
