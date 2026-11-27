use std::sync::Arc;
use std::thread;

use domain::{Site, SiteId, Torrent};
use parking_lot::RwLock;

use crate::fetch::{FetchRequest, Fetcher};
use crate::profile::Profile;
use crate::provider::{
    ProviderAvailability, ProviderCallContext, ProviderCapability, ResolvedDownload, SearchQuery,
    SiteFetch, SiteRateLimiter,
};
use crate::registry::{RegistryCandidate, RegistrySnapshot};
use crate::{IndexerError, RegistryError, SiteFailureKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteFailure {
    pub site_id: SiteId,
    pub provider_id: Option<String>,
    pub generation: u64,
    pub kind: SiteFailureKind,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub generation: u64,
    pub torrents: Vec<Torrent>,
    pub failures: Vec<SiteFailure>,
}

pub struct Indexer {
    snapshot: RwLock<Arc<RegistrySnapshot>>,
    fetcher: Arc<dyn Fetcher>,
    rate_limiter: Arc<SiteRateLimiter>,
}

#[derive(Clone, Copy)]
enum Mode<'a> {
    Search {
        keyword: &'a str,
        page: u32,
        categories: &'a [String],
    },
    Rss,
}

impl SiteFetch for Indexer {
    fn fetch(&self, site: &Site, request: &FetchRequest) -> Result<String, IndexerError> {
        self.rate_limiter.wait_if_needed(site);
        self.fetcher.fetch(request)
    }
}

impl Indexer {
    pub fn new(snapshot: Arc<RegistrySnapshot>, fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            fetcher,
            rate_limiter: Arc::new(SiteRateLimiter::new()),
        }
    }

    pub fn snapshot(&self) -> Arc<RegistrySnapshot> {
        Arc::clone(&self.snapshot.read())
    }

    pub fn rate_limiter(&self) -> &Arc<SiteRateLimiter> {
        &self.rate_limiter
    }

    pub fn publish(
        &self,
        candidate: RegistryCandidate,
    ) -> Result<Arc<RegistrySnapshot>, RegistryError> {
        let mut guard = self.snapshot.write();
        let next_gen = guard.generation().saturating_add(1);
        let next = candidate.build_with_generation(next_gen)?;
        *guard = Arc::clone(&next);
        Ok(next)
    }

    pub fn publish_if_generation(
        &self,
        expected_generation: u64,
        candidate: RegistryCandidate,
    ) -> Result<Arc<RegistrySnapshot>, RegistryError> {
        let mut guard = self.snapshot.write();
        let actual = guard.generation();
        if actual != expected_generation {
            return Err(RegistryError::StaleExpectedGeneration {
                expected: expected_generation,
                actual,
            });
        }
        let next_gen = actual.saturating_add(1);
        let next = candidate.build_with_generation(next_gen)?;
        *guard = Arc::clone(&next);
        Ok(next)
    }

    pub fn profile(&self, id: &str) -> Option<Arc<Profile>> {
        self.snapshot().profile(id)
    }

    pub fn profile_ids(&self) -> Vec<String> {
        self.snapshot().profile_ids()
    }

    pub fn search(&self, sites: &[Site], keyword: &str) -> SearchOutcome {
        self.search_page(sites, keyword, 1)
    }

    pub fn search_page(&self, sites: &[Site], keyword: &str, page: u32) -> SearchOutcome {
        self.search_page_categories(sites, keyword, page, &[])
    }

    pub fn search_page_categories(
        &self,
        sites: &[Site],
        keyword: &str,
        page: u32,
        categories: &[String],
    ) -> SearchOutcome {
        let snapshot = self.snapshot();
        self.search_page_categories_with_snapshot(&snapshot, sites, keyword, page, categories)
    }

    pub fn search_with_snapshot(
        &self,
        snapshot: &RegistrySnapshot,
        sites: &[Site],
        keyword: &str,
    ) -> SearchOutcome {
        self.search_page_categories_with_snapshot(snapshot, sites, keyword, 1, &[])
    }

    pub fn search_page_with_snapshot(
        &self,
        snapshot: &RegistrySnapshot,
        sites: &[Site],
        keyword: &str,
        page: u32,
    ) -> SearchOutcome {
        self.search_page_categories_with_snapshot(snapshot, sites, keyword, page, &[])
    }

    pub fn search_page_categories_with_snapshot(
        &self,
        snapshot: &RegistrySnapshot,
        sites: &[Site],
        keyword: &str,
        page: u32,
        categories: &[String],
    ) -> SearchOutcome {
        self.run(
            snapshot,
            sites,
            Mode::Search {
                keyword,
                page: page.max(1),
                categories,
            },
        )
    }

    pub fn rss(&self, sites: &[Site]) -> SearchOutcome {
        let snapshot = self.snapshot();
        self.rss_with_snapshot(&snapshot, sites)
    }

    pub fn rss_with_snapshot(&self, snapshot: &RegistrySnapshot, sites: &[Site]) -> SearchOutcome {
        self.run(snapshot, sites, Mode::Rss)
    }

    pub fn fetch_user_stats(&self, site: &Site) -> Result<Option<(u64, u64)>, IndexerError> {
        let snapshot = self.snapshot();
        let entry = snapshot
            .indexer(&site.profile_id)
            .ok_or_else(|| IndexerError::UnknownProfile(site.profile_id.clone()))?;
        if let ProviderAvailability::Disabled { reason } = &entry.descriptor().availability {
            return Err(IndexerError::ProviderDisabled {
                provider_id: entry.descriptor().id.clone(),
                reason: reason.clone(),
            });
        }
        let ctx = ProviderCallContext {
            generation: snapshot.generation(),
            fetcher: self,
        };
        if entry
            .descriptor()
            .capabilities
            .contains(&ProviderCapability::UserStats)
        {
            let stats = entry.provider().user_stats(&ctx, site)?;
            return Ok(match (stats.uploaded, stats.downloaded) {
                (Some(u), Some(d)) => Some((u, d)),
                _ => None,
            });
        }
        let profile = entry
            .profile()
            .ok_or_else(|| IndexerError::UnsupportedCapability {
                provider_id: entry.descriptor().id.clone(),
                capability: ProviderCapability::UserStats.as_str().into(),
            })?;
        let html_fallback = crate::fetch::uses_html_cookie_fallback(site, profile);
        if profile.framework != crate::Framework::Nexusphp && !html_fallback {
            return Ok(None);
        }
        if site.cookie.as_deref().is_none_or(|c| c.trim().is_empty()) {
            return Ok(None);
        }
        let base = crate::fetch::effective_base_url(site, profile);
        let req = FetchRequest {
            key: format!("user_stats:{}", site.id),
            url: format!("{}/index.php", base.trim_end_matches('/')),
            method: crate::FetchMethod::Get,
            body: None,
            cookie: site.cookie.clone(),
            api_key: None,
            proxy: site.proxy.clone(),
            cdp_url: site.cdp_url.clone(),
        };
        let body = self.fetch(site, &req)?;
        crate::user_stats::parse_nexusphp_user_stats(&body)
    }

    pub fn resolve_torrent_download(
        &self,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        let snapshot = self.snapshot();
        self.resolve_torrent_download_at(site, torrent, snapshot.generation())
    }

    pub fn resolve_torrent_download_at(
        &self,
        site: &Site,
        torrent: &Torrent,
        expected_generation: u64,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        if site.id != torrent.site_id {
            return Err(IndexerError::InvalidInput(format!(
                "site id {} does not match torrent site_id {}",
                site.id, torrent.site_id
            )));
        }
        let snapshot = self.snapshot();
        let actual = snapshot.generation();
        if actual != expected_generation {
            return Err(IndexerError::StaleGeneration {
                expected: expected_generation,
                actual,
            });
        }
        let entry = snapshot
            .indexer(&site.profile_id)
            .ok_or_else(|| IndexerError::UnknownProfile(site.profile_id.clone()))?;
        if let ProviderAvailability::Disabled { reason } = &entry.descriptor().availability {
            return Err(IndexerError::ProviderDisabled {
                provider_id: entry.descriptor().id.clone(),
                reason: reason.clone(),
            });
        }
        if !entry
            .descriptor()
            .capabilities
            .contains(&ProviderCapability::ResolveDownload)
        {
            return Ok(None);
        }
        let ctx = ProviderCallContext {
            generation: actual,
            fetcher: self,
        };
        entry.provider().resolve_download(&ctx, site, torrent)
    }

    fn run(&self, snapshot: &RegistrySnapshot, sites: &[Site], mode: Mode<'_>) -> SearchOutcome {
        let generation = snapshot.generation();
        let mut torrents = Vec::new();
        let mut failures = Vec::new();
        thread::scope(|scope| {
            let mut joins = Vec::new();
            for site in sites {
                joins.push((site, scope.spawn(|| self.one(snapshot, site, mode))));
            }
            for (site, join) in joins {
                match join
                    .join()
                    .unwrap_or_else(|_| Err(IndexerError::Fetch("search thread panicked".into())))
                {
                    Ok(mut found) => torrents.append(&mut found),
                    Err(err) => failures.push(SiteFailure {
                        site_id: site.id,
                        provider_id: Some(site.profile_id.clone()),
                        generation,
                        kind: err.failure_kind(),
                        error: err.to_string(),
                    }),
                }
            }
        });
        SearchOutcome {
            generation,
            torrents,
            failures,
        }
    }

    fn one(
        &self,
        snapshot: &RegistrySnapshot,
        site: &Site,
        mode: Mode<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let entry = snapshot
            .indexer(&site.profile_id)
            .ok_or_else(|| IndexerError::UnknownProfile(site.profile_id.clone()))?;
        let desc = entry.descriptor();
        if let ProviderAvailability::Disabled { reason } = &desc.availability {
            return Err(IndexerError::ProviderDisabled {
                provider_id: desc.id.clone(),
                reason: reason.clone(),
            });
        }
        let required_cap = match mode {
            Mode::Search { .. } => ProviderCapability::Search,
            Mode::Rss => ProviderCapability::Rss,
        };
        if !desc.capabilities.contains(&required_cap) {
            return Err(IndexerError::UnsupportedCapability {
                provider_id: desc.id.clone(),
                capability: required_cap.as_str().into(),
            });
        }

        let ctx = ProviderCallContext {
            generation: snapshot.generation(),
            fetcher: self,
        };
        let results = match mode {
            Mode::Search {
                keyword,
                page,
                categories,
            } => {
                tracing::debug!(site = %site.name, keyword = %keyword, "正在索引站点搜索");
                let query = SearchQuery {
                    keyword,
                    page,
                    categories,
                };
                entry.provider().search(&ctx, site, &query)
            }
            Mode::Rss => {
                tracing::debug!(site = %site.name, "正在拉取索引站点 RSS");
                entry.provider().rss(&ctx, site, None)
            }
        };
        match &results {
            Ok(torrents) => {
                tracing::info!(site = %site.name, count = torrents.len(), "索引站点返回种子");
            }
            Err(error) => {
                tracing::error!(site = %site.name, error = %error, "索引站点执行失败");
            }
        }
        results
    }
}
