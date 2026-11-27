use std::sync::Arc;

use domain::{Site, Torrent};

use crate::fetch::{
    download_token_request, effective_base_url, mteam_download_request, rss_request,
    search_request_page_categories, uses_html_cookie_fallback,
};
use crate::parse;
use crate::profile::{ApiDialect, Framework, Profile};
use crate::provider::{
    AuthType, IndexerProvider, ProviderAvailability, ProviderCallContext, ProviderCapability,
    ProviderDescriptor, ProviderSource, ResolvedDownload, SearchQuery,
};
use crate::{IndexerError, RegistryError};

fn map_profile_auth_types(profile: &Profile) -> Vec<AuthType> {
    let mut out = Vec::new();
    for raw in profile.effective_auth_types() {
        match raw {
            "apikey" => out.push(AuthType::ApiKey),
            "cookie" => out.push(AuthType::Cookie),
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(if profile.framework == Framework::Api {
            AuthType::ApiKey
        } else {
            AuthType::Cookie
        });
    }
    out
}

pub struct NexusPhpYamlProvider {
    profile: Arc<Profile>,
    descriptor: ProviderDescriptor,
}

impl NexusPhpYamlProvider {
    pub fn new(profile: Arc<Profile>, source: ProviderSource) -> Self {
        let mut capabilities = vec![ProviderCapability::Search];
        if profile.rss.is_some() {
            capabilities.push(ProviderCapability::Rss);
        }
        let hidden_from_catalog = profile.id == "demo";
        let descriptor = ProviderDescriptor {
            id: profile.id.clone(),
            name: profile.id.clone(),
            kind: "nexusphp".into(),
            source,
            auth_types: map_profile_auth_types(&profile),
            capabilities,
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog,
        };
        Self {
            profile,
            descriptor,
        }
    }
}

impl IndexerProvider for NexusPhpYamlProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn profile(&self) -> Option<&Arc<Profile>> {
        Some(&self.profile)
    }

    fn search(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = search_request_page_categories(
            site,
            &self.profile,
            query.keyword,
            query.page,
            query.categories,
        );
        let body = ctx.fetcher.fetch(site, &req)?;
        parse::search(site, &self.profile, &body)
    }

    fn rss(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        _cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = rss_request(site, &self.profile);
        let body = ctx.fetcher.fetch(site, &req)?;
        parse::rss(site, &self.profile, &body)
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

pub struct MTeamProvider {
    profile: Arc<Profile>,
    descriptor: ProviderDescriptor,
}

impl MTeamProvider {
    pub fn new(profile: Arc<Profile>, source: ProviderSource) -> Self {
        let mut capabilities = vec![
            ProviderCapability::Search,
            ProviderCapability::ResolveDownload,
        ];
        if profile.rss.is_some() {
            capabilities.push(ProviderCapability::Rss);
        }
        let descriptor = ProviderDescriptor {
            id: profile.id.clone(),
            name: profile.id.clone(),
            kind: "mteam-api".into(),
            source,
            auth_types: vec![AuthType::ApiKey],
            capabilities,
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        };
        Self {
            profile,
            descriptor,
        }
    }
}

impl IndexerProvider for MTeamProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn profile(&self) -> Option<&Arc<Profile>> {
        Some(&self.profile)
    }

    fn search(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = search_request_page_categories(
            site,
            &self.profile,
            query.keyword,
            query.page,
            query.categories,
        );
        let body = ctx.fetcher.fetch(site, &req)?;
        let items = parse::parse_mteam_items(&body)?;
        let base = site.url.trim_end_matches('/');
        Ok(items
            .into_iter()
            .map(|item| {
                let id_str = item.id.clone().unwrap_or_default();
                let enclosure = format!("{base}/api/torrent/download?id={id_str}");
                item.into_torrent(site.id, enclosure, self.profile.categories.as_ref())
            })
            .collect())
    }

    fn rss(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        _cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = rss_request(site, &self.profile);
        let body = ctx.fetcher.fetch(site, &req)?;
        parse::rss(site, &self.profile, &body)
    }

    fn resolve_download(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        let id = torrent
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .or_else(|| parse::extract_id(&torrent.enclosure))
            .ok_or_else(|| {
                IndexerError::InvalidInput("API Torrent 缺少 ID，无法获取下载令牌".into())
            })?;
        let req = download_token_request(site, &self.profile, &id)
            .unwrap_or_else(|| mteam_download_request(site, &id));
        let body = ctx.fetcher.fetch(site, &req)?;
        parse::parse_mteam_download(&body).map(|url| Some(ResolvedDownload::Url(url)))
    }
}

pub struct GenericApiYamlProvider {
    profile: Arc<Profile>,
    descriptor: ProviderDescriptor,
}

impl GenericApiYamlProvider {
    pub fn new(profile: Arc<Profile>, source: ProviderSource) -> Self {
        let mut capabilities = vec![ProviderCapability::Search];
        if profile.rss.is_some() {
            capabilities.push(ProviderCapability::Rss);
        }
        if profile.download.is_some() {
            capabilities.push(ProviderCapability::ResolveDownload);
        }
        let descriptor = ProviderDescriptor {
            id: profile.id.clone(),
            name: profile.id.clone(),
            kind: "custom-api".into(),
            source,
            auth_types: map_profile_auth_types(&profile),
            capabilities,
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        };
        Self {
            profile,
            descriptor,
        }
    }
}

impl IndexerProvider for GenericApiYamlProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn profile(&self) -> Option<&Arc<Profile>> {
        Some(&self.profile)
    }

    fn search(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = search_request_page_categories(
            site,
            &self.profile,
            query.keyword,
            query.page,
            query.categories,
        );
        let body = ctx.fetcher.fetch(site, &req)?;
        if uses_html_cookie_fallback(site, &self.profile) {
            return parse::parse_html(site, &self.profile, &body);
        }
        match self.profile.api_dialect {
            ApiDialect::Mteam => {
                let items = parse::parse_mteam_items(&body)?;
                let base = site.url.trim_end_matches('/');
                Ok(items
                    .into_iter()
                    .map(|item| {
                        let id_str = item.id.clone().unwrap_or_default();
                        let enclosure = format!("{base}/api/torrent/download?id={id_str}");
                        item.into_torrent(site.id, enclosure, self.profile.categories.as_ref())
                    })
                    .collect())
            }
            ApiDialect::HdareaV1 => parse::parse_hdarea_torrents(site, &self.profile, &body),
        }
    }

    fn rss(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        _cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        let req = rss_request(site, &self.profile);
        let body = ctx.fetcher.fetch(site, &req)?;
        parse::rss(site, &self.profile, &body)
    }

    fn resolve_download(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        if uses_html_cookie_fallback(site, &self.profile) {
            return Ok(None);
        }
        if self.profile.api_dialect == ApiDialect::HdareaV1
            && (self.profile.download.is_none()
                || torrent.enclosure.contains("dl_token=")
                || torrent.enclosure.contains("passkey="))
        {
            return Ok(None);
        }
        let id = torrent
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .or_else(|| parse::extract_id(&torrent.enclosure));
        let Some(id) = id else {
            return Err(IndexerError::InvalidInput(
                "API Torrent 缺少 ID，无法获取下载令牌".into(),
            ));
        };
        let Some(req) = download_token_request(site, &self.profile, &id) else {
            return Err(IndexerError::UnsupportedCapability {
                provider_id: self.descriptor.id.clone(),
                capability: "resolve-download".into(),
            });
        };
        let body = ctx.fetcher.fetch(site, &req)?;
        match self.profile.api_dialect {
            ApiDialect::Mteam => {
                parse::parse_mteam_download(&body).map(|url| Some(ResolvedDownload::Url(url)))
            }
            ApiDialect::HdareaV1 => {
                let base = effective_base_url(site, &self.profile);
                parse::parse_hdarea_download(&base, &body)
                    .map(|url| Some(ResolvedDownload::Url(url)))
            }
        }
    }
}

pub(crate) fn provider_from_profile(
    profile: Arc<Profile>,
    source: ProviderSource,
) -> Result<Arc<dyn IndexerProvider>, RegistryError> {
    match profile.framework {
        Framework::Nexusphp => Ok(Arc::new(NexusPhpYamlProvider::new(profile, source))),
        Framework::Api if profile.id == "mteam" && !source.is_plugin() => {
            Ok(Arc::new(MTeamProvider::new(profile, source)))
        }
        Framework::Api => Ok(Arc::new(GenericApiYamlProvider::new(profile, source))),
    }
}
