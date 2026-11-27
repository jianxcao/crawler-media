use crate::indexer_bridge_map::{
    effective_plugin_generation, map_api_error, map_failure, map_resolved_download, map_torrent,
};
use crate::{CompiledIndexer, CompiledMaintenance, Invocation, PluginHost};
use domain::{Site, Torrent};
use indexer::{
    IndexerError, IndexerProvider, MaintenanceAction, MaintenanceCallContext,
    MaintenanceDescriptor, MaintenanceOutcome, MaintenanceProvider, MaintenanceState,
    ProviderCallContext, ProviderCapability, ProviderCategory, ProviderDescriptor,
    ResolvedDownload, SearchQuery, SiteRateLimiter, UserStats,
};
use plugin_api::{self as api, ErrorCode, GuestRole, PluginTrust};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone)]
pub struct WasmIndexerConfig {
    pub plugin_id: String,
    pub provider_id: String,
    pub descriptor: ProviderDescriptor,
    pub trust: PluginTrust,
    pub rate_limiter: Option<Arc<SiteRateLimiter>>,
}

#[derive(Clone)]
pub struct WasmMaintenanceConfig {
    pub plugin_id: String,
    pub provider_id: String,
    pub descriptor: MaintenanceDescriptor,
    pub trust: PluginTrust,
    pub rate_limiter: Option<Arc<SiteRateLimiter>>,
}

pub struct WasmIndexerProvider {
    host: PluginHost,
    compiled: CompiledIndexer,
    config: WasmIndexerConfig,
}

impl WasmIndexerProvider {
    pub fn new(
        host: PluginHost,
        compiled: CompiledIndexer,
        config: WasmIndexerConfig,
    ) -> Result<Self, IndexerError> {
        let mut inv = Invocation::new(config.plugin_id.clone(), host.core.limits.wall);
        inv.provider_id = Some(config.provider_id.clone());
        inv.generation = effective_plugin_generation(&config.descriptor.source, 1);
        inv.role = GuestRole::Lifecycle;
        inv.trust = config.trust;
        let guest_descriptors = host
            .indexer_describe(&compiled, inv.clone())
            .map_err(|e| map_failure(&config.descriptor.id, "describe", inv.generation, e))?;
        let Some(matched) = guest_descriptors
            .iter()
            .find(|d| d.id == config.provider_id || d.id == config.descriptor.id)
        else {
            return Err(IndexerError::InvalidInput(format!(
                "plugin `{}` did not describe provider `{}`",
                config.plugin_id, config.provider_id
            )));
        };
        let expected_caps: HashSet<&str> = config
            .descriptor
            .capabilities
            .iter()
            .map(|c| c.as_str())
            .collect();
        let guest_caps: HashSet<&str> = matched.capabilities.iter().map(String::as_str).collect();
        if expected_caps != guest_caps {
            return Err(IndexerError::InvalidInput(format!(
                "provider `{}` descriptor capabilities mismatch manifest",
                config.descriptor.id
            )));
        }
        Ok(Self {
            host,
            compiled,
            config,
        })
    }

    fn require_cap(&self, cap: ProviderCapability) -> Result<(), IndexerError> {
        if self.config.descriptor.capabilities.contains(&cap) {
            Ok(())
        } else {
            Err(IndexerError::UnsupportedCapability {
                provider_id: self.config.descriptor.id.clone(),
                capability: cap.as_str().into(),
            })
        }
    }

    fn build_invocation(
        &self,
        site: &Site,
        call_generation: u64,
    ) -> Result<Invocation, IndexerError> {
        let generation =
            effective_plugin_generation(&self.config.descriptor.source, call_generation);
        let site_id = site.id.to_string();
        let epoch = match self.host.ports.site_credential_epoch(&site_id) {
            Ok(ep) => ep,
            Err(err) if err.code == ErrorCode::Unsupported => None,
            Err(err) => {
                return Err(map_api_error(
                    &self.config.descriptor.id,
                    "",
                    generation,
                    err,
                ));
            }
        };
        let mut inv = Invocation::new(
            self.config.plugin_id.clone(),
            self.host.core.limits.provider_wall,
        );
        inv.provider_id = Some(self.config.provider_id.clone());
        inv.generation = generation;
        inv.role = GuestRole::Indexer;
        inv.site_id = Some(site_id);
        inv.credential_epoch = epoch;
        inv.trust = self.config.trust;
        inv.max_http_calls = self.host.core.limits.max_http_calls;
        Ok(inv)
    }

    fn wait_rate_limit(&self, site: &Site) {
        if let Some(limiter) = &self.config.rate_limiter {
            limiter.wait_if_needed(site);
        }
    }
}

impl IndexerProvider for WasmIndexerProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.config.descriptor
    }

    fn search(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        query: &SearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.require_cap(ProviderCapability::Search)?;
        self.wait_rate_limit(site);
        let inv = self.build_invocation(site, ctx.generation)?;
        let api_query = api::SearchQuery {
            keyword: query.keyword.to_string(),
            page: query.page,
            categories: query.categories.to_vec(),
        };
        let batch = self
            .host
            .indexer_search(
                &self.compiled,
                inv.clone(),
                &self.config.provider_id,
                api_query,
            )
            .map_err(|e| map_failure(&self.config.descriptor.id, "search", inv.generation, e))?;
        Ok(batch
            .items
            .into_iter()
            .map(|raw| map_torrent(site, raw))
            .collect())
    }

    fn rss(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        cursor: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.require_cap(ProviderCapability::Rss)?;
        self.wait_rate_limit(site);
        let inv = self.build_invocation(site, ctx.generation)?;
        let batch = self
            .host
            .indexer_rss(
                &self.compiled,
                inv.clone(),
                &self.config.provider_id,
                cursor.map(str::to_string),
            )
            .map_err(|e| map_failure(&self.config.descriptor.id, "rss", inv.generation, e))?;
        Ok(batch
            .items
            .into_iter()
            .map(|raw| map_torrent(site, raw))
            .collect())
    }

    fn resolve_download(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        torrent: &Torrent,
    ) -> Result<Option<ResolvedDownload>, IndexerError> {
        self.require_cap(ProviderCapability::ResolveDownload)?;
        if torrent.enclosure.trim().is_empty() {
            return Ok(None);
        }
        let inv = self.build_invocation(site, ctx.generation)?;
        let (req, site_auth_granted) = self
            .host
            .indexer_resolve_download_with_auth(
                &self.compiled,
                inv.clone(),
                &self.config.provider_id,
                torrent.enclosure.clone(),
            )
            .map_err(|e| {
                map_failure(
                    &self.config.descriptor.id,
                    "resolve-download",
                    inv.generation,
                    e,
                )
            })?;
        map_resolved_download(
            site,
            &self.config.descriptor.id,
            site_auth_granted,
            &inv,
            req,
        )
        .map(Some)
    }

    fn detail(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
        torrent_ref: &str,
    ) -> Result<Torrent, IndexerError> {
        self.require_cap(ProviderCapability::Detail)?;
        self.wait_rate_limit(site);
        let inv = self.build_invocation(site, ctx.generation)?;
        let raw = self
            .host
            .indexer_detail(
                &self.compiled,
                inv.clone(),
                &self.config.provider_id,
                torrent_ref.to_string(),
            )
            .map_err(|e| map_failure(&self.config.descriptor.id, "detail", inv.generation, e))?;
        Ok(map_torrent(site, raw))
    }

    fn user_stats(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
    ) -> Result<UserStats, IndexerError> {
        self.require_cap(ProviderCapability::UserStats)?;
        self.wait_rate_limit(site);
        let inv = self.build_invocation(site, ctx.generation)?;
        let stats = self
            .host
            .indexer_user_stats(&self.compiled, inv.clone())
            .map_err(|e| {
                map_failure(&self.config.descriptor.id, "user-stats", inv.generation, e)
            })?;
        Ok(UserStats {
            uploaded: stats.uploaded,
            downloaded: stats.downloaded,
        })
    }

    fn categories(
        &self,
        ctx: &ProviderCallContext<'_>,
        site: &Site,
    ) -> Result<Vec<ProviderCategory>, IndexerError> {
        self.require_cap(ProviderCapability::Categories)?;
        self.wait_rate_limit(site);
        let inv = self.build_invocation(site, ctx.generation)?;
        let cats = self
            .host
            .indexer_categories(&self.compiled, inv.clone())
            .map_err(|e| {
                map_failure(&self.config.descriptor.id, "categories", inv.generation, e)
            })?;
        Ok(cats
            .into_iter()
            .map(|c| ProviderCategory {
                id: c.id,
                name: c.name,
            })
            .collect())
    }
}

pub struct WasmMaintenanceProvider {
    host: PluginHost,
    compiled: CompiledMaintenance,
    config: WasmMaintenanceConfig,
}

impl WasmMaintenanceProvider {
    pub fn new(
        host: PluginHost,
        compiled: CompiledMaintenance,
        config: WasmMaintenanceConfig,
    ) -> Self {
        Self {
            host,
            compiled,
            config,
        }
    }
}

impl MaintenanceProvider for WasmMaintenanceProvider {
    fn descriptor(&self) -> &MaintenanceDescriptor {
        &self.config.descriptor
    }

    fn run(
        &self,
        ctx: &MaintenanceCallContext<'_>,
        site: &Site,
        action: MaintenanceAction,
        job_id: &str,
    ) -> Result<MaintenanceOutcome, IndexerError> {
        if !self.config.descriptor.actions.contains(&action) {
            return Err(IndexerError::UnsupportedCapability {
                provider_id: self.config.descriptor.id.clone(),
                capability: action.as_str().into(),
            });
        }
        if let Some(limiter) = &self.config.rate_limiter {
            limiter.wait_if_needed(site);
        }
        let generation =
            effective_plugin_generation(&self.config.descriptor.source, ctx.generation);
        let site_id = site.id.to_string();
        let epoch = match self.host.ports.site_credential_epoch(&site_id) {
            Ok(ep) => ep,
            Err(err) if err.code == ErrorCode::Unsupported => None,
            Err(err) => {
                return Err(map_api_error(
                    &self.config.descriptor.id,
                    action.as_str(),
                    generation,
                    err,
                ));
            }
        };
        let mut inv = Invocation::new(
            self.config.plugin_id.clone(),
            self.host.core.limits.provider_wall,
        );
        inv.provider_id = Some(self.config.provider_id.clone());
        inv.generation = generation;
        inv.role = GuestRole::Maintenance;
        inv.site_id = Some(site_id);
        inv.credential_epoch = epoch;
        inv.trust = self.config.trust;
        inv.max_http_calls = self.host.core.limits.max_http_calls;
        let api_action = match action {
            MaintenanceAction::VerifySession => api::MaintenanceAction::VerifySession,
            MaintenanceAction::CheckIn => api::MaintenanceAction::CheckIn,
        };
        let out = self
            .host
            .maintenance_run(&self.compiled, inv, api_action, job_id.to_string())
            .map_err(|e| map_failure(&self.config.descriptor.id, action.as_str(), generation, e))?;
        Ok(MaintenanceOutcome {
            state: match out.state {
                api::MaintenanceState::Completed => MaintenanceState::Completed,
                api::MaintenanceState::AlreadyCompleted => MaintenanceState::AlreadyCompleted,
                api::MaintenanceState::AuthExpired => MaintenanceState::AuthExpired,
                api::MaintenanceState::Challenge => MaintenanceState::Challenge,
                api::MaintenanceState::Unknown => MaintenanceState::Unknown,
            },
            evidence: out.evidence,
            session_capture: out.session_capture,
        })
    }
}
