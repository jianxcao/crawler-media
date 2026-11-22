mod browser;
pub mod browser_host;
pub mod cdp;
pub mod cdp_page;
mod engine;
mod error;
mod fetch;
mod parse;
mod profile;

pub use browser::{
    Browser, BrowserConfig, BrowserRoute, PageSession, RecordingFetcher, RenderedPage,
};
pub use cdp::{CdpCookie, DiscoveredSiteCookie, fetch_cookies_from_cdp, match_cookies_to_profiles};
pub use engine::{Indexer, SearchOutcome, SiteFailure};
pub use error::IndexerError;
pub use fetch::{FetchMethod, FetchRequest, Fetcher};
pub use profile::{Framework, Profile, ProfileSet};
