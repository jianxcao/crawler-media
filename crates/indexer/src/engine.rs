use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use domain::{Site, SiteId, Torrent};
use parking_lot::Mutex;

use crate::IndexerError;
use crate::fetch::{
    FetchRequest, Fetcher, download_token_request, mteam_download_request, rss_request,
};
use crate::parse;
use crate::profile::{Framework, Profile, ProfileSet};

#[derive(Debug)]
pub struct SiteFailure {
    pub site_id: SiteId,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub torrents: Vec<Torrent>,
    pub failures: Vec<SiteFailure>,
}

pub struct Indexer {
    profiles: ProfileSet,
    fetcher: Arc<dyn Fetcher>,
    next_request: Mutex<HashMap<SiteId, Instant>>,
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

impl Indexer {
    pub fn new(profiles: ProfileSet, fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            profiles,
            fetcher,
            next_request: Mutex::new(HashMap::new()),
        }
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id)
    }

    /// 可用 profile id 清单（sites/catalog 用）。
    pub fn profile_ids(&self) -> Vec<String> {
        self.profiles.ids()
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
        self.run(
            sites,
            Mode::Search {
                keyword,
                page: page.max(1),
                categories,
            },
        )
    }

    pub fn rss(&self, sites: &[Site]) -> SearchOutcome {
        self.run(sites, Mode::Rss)
    }

    /// Resolve an API Site's short-lived Torrent URL only when it is about to
    /// be delivered. The search result keeps its stable enclosure as identity.
    pub fn resolve_torrent_download(
        &self,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<String>, IndexerError> {
        let profile = self
            .profiles
            .get(&site.profile_id)
            .ok_or_else(|| IndexerError::UnknownProfile(site.profile_id.clone()))?;
        if profile.framework != Framework::Api {
            return Ok(None);
        }
        let id = torrent
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| IndexerError::Parse("API Torrent 缺少 ID，无法获取下载令牌".into()))?;
        self.resolve_download(site, profile, id).map(Some)
    }

    fn run(&self, sites: &[Site], mode: Mode<'_>) -> SearchOutcome {
        let mut torrents = Vec::new();
        let mut failures = Vec::new();
        thread::scope(|scope| {
            let mut joins = Vec::new();
            for site in sites {
                joins.push((site.id, scope.spawn(|| self.one(site, mode))));
            }
            for (site_id, join) in joins {
                match join
                    .join()
                    .unwrap_or_else(|_| Err(IndexerError::Fetch("search thread panicked".into())))
                {
                    Ok(mut found) => torrents.append(&mut found),
                    Err(err) => failures.push(SiteFailure {
                        site_id,
                        error: err.to_string(),
                    }),
                }
            }
        });
        SearchOutcome { torrents, failures }
    }

    fn one(&self, site: &Site, mode: Mode<'_>) -> Result<Vec<Torrent>, IndexerError> {
        let profile = self
            .profiles
            .get(&site.profile_id)
            .ok_or_else(|| IndexerError::UnknownProfile(site.profile_id.clone()))?;
        let request = match mode {
            Mode::Search {
                keyword,
                page,
                categories,
            } => {
                tracing::debug!(site = %site.name, keyword = %keyword, "正在索引站点搜索");
                crate::fetch::search_request_page_categories(
                    site, profile, keyword, page, categories,
                )
            }
            Mode::Rss => {
                tracing::debug!(site = %site.name, "正在拉取索引站点 RSS");
                rss_request(site, profile)
            }
        };
        let body = self.fetch(site, &request).map_err(|e| {
            tracing::error!(site = %site.name, error = %e, "拉取索引站点响应失败");
            e
        })?;
        let results = match mode {
            Mode::Search { .. } if profile.framework == Framework::Api => {
                let items = parse::parse_mteam_items(&body)?;
                // 不在搜索阶段逐个触发 genDlToken（搜索可能返回上百个种子，连续调 100 次
                // 会直接触发馒头的风控「請求過於頻繁」并耗尽 API 配额，且耗时高达几十秒）。
                // 这里用占位或种子详情/ID 链接作为 enclosure，在真正投递或下载时按需调用，
                // 或者在没有报错时保留已有的成功项。
                let mut torrents = Vec::new();
                for item in items {
                    let id_str = item.id.clone().unwrap_or_default();
                    let enclosure = format!(
                        "{}/api/torrent/download?id={id_str}",
                        site.url.trim_end_matches('/')
                    );
                    torrents.push(item.into_torrent(site.id, enclosure));
                }
                Ok(torrents)
            }
            Mode::Search { .. } => parse::search(site, profile, &body),
            Mode::Rss => parse::rss(site, profile, &body),
        };
        match &results {
            Ok(torrents) => {
                tracing::info!(site = %site.name, count = torrents.len(), "索引站点返回种子");
            }
            Err(error) => {
                tracing::error!(site = %site.name, error = %error, "解析索引站点响应失败");
            }
        }
        results
    }

    fn resolve_download(
        &self,
        site: &Site,
        profile: &Profile,
        id: &str,
    ) -> Result<String, IndexerError> {
        let request = download_token_request(site, profile, id)
            .unwrap_or_else(|| mteam_download_request(site, id));
        let body = self.fetch(site, &request)?;
        parse::parse_mteam_download(&body)
    }

    fn fetch(&self, site: &Site, request: &FetchRequest) -> Result<String, IndexerError> {
        if let Some(limit) = site.rate_limit_per_minute.filter(|limit| *limit > 0) {
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
        self.fetcher.fetch(request)
    }
}
