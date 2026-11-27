use std::str::FromStr;

use domain::{Site, SiteId};
use plugin_api::{ApiError, ErrorCode, GuestRole, PageRequest, SitePage, SiteView, StatsView};
use plugin_host::{HostPorts, Invocation};
use store::plugins::{PluginGrant, PluginGrantScope};

use super::{ActiveAuth, HostDeps, StoreHostPorts, api_err, map_store_error};

pub(crate) fn strip_kind_prefix<'a>(raw: &'a str, kind: &str) -> &'a str {
    let trimmed = raw.trim();
    if trimmed.len() > kind.len()
        && trimmed
            .get(..kind.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(kind))
        && trimmed.as_bytes().get(kind.len()) == Some(&b':')
    {
        trimmed[kind.len() + 1..].trim()
    } else {
        trimmed
    }
}

pub(crate) fn strip_site_prefix(handle: &str) -> &str {
    strip_kind_prefix(handle, "site")
}

pub(crate) fn parse_site_scope_id(id: &str) -> Option<SiteId> {
    SiteId::from_str(strip_site_prefix(id)).ok()
}

pub(crate) fn parse_downloader_scope_id(id: &str) -> Option<domain::DownloaderId> {
    domain::DownloaderId::from_str(strip_kind_prefix(id, "downloader")).ok()
}

pub(crate) fn parse_library_scope_id(id: &str) -> Option<domain::LibraryId> {
    domain::LibraryId::from_str(strip_kind_prefix(id, "library")).ok()
}

pub(crate) fn parse_subscribe_scope_id(id: &str) -> Option<domain::SubscribeId> {
    domain::SubscribeId::from_str(strip_kind_prefix(id, "subscribe")).ok()
}

fn parse_site_handle(handle: &str) -> Result<SiteId, ApiError> {
    let normalized = strip_site_prefix(handle);
    if normalized.is_empty() {
        return Err(api_err(
            ErrorCode::InvalidArgument,
            "site handle must not be empty",
        ));
    }
    SiteId::from_str(normalized)
        .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid site handle"))
}

fn has_read_sites_grant_for(grants: &[PluginGrant], site_id: SiteId) -> bool {
    grants.iter().any(|g| {
        g.capability == "read:sites"
            && match &g.scope {
                PluginGrantScope::Global => true,
                PluginGrantScope::Site(id) => *id == site_id,
                PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
                    parse_site_scope_id(id) == Some(site_id)
                }
                _ => false,
            }
    })
}

fn redacted_site_view(site: &Site) -> SiteView {
    let mut capabilities = vec!["search".to_string()];
    if site.browser_search {
        capabilities.push("browser-search".to_string());
    }
    SiteView {
        handle: site.id.to_string(),
        id: site.id.to_string(),
        name: site.name.clone(),
        enabled: site.enabled,
        profile_ref: site.profile_id.clone(),
        capabilities,
        maintenance_policy: site.maintenance_via.as_str().to_string(),
    }
}

pub(super) fn site_list(
    ports: &StoreHostPorts,
    inv: &Invocation,
    page: PageRequest,
) -> Result<SitePage, ApiError> {
    if inv.role == GuestRole::Lifecycle {
        return Err(api_err(
            ErrorCode::WrongContext,
            "site port is forbidden during lifecycle validation",
        ));
    }
    let ctx = ports.lookup(inv)?;
    let ActiveAuth::Runtime {
        authority,
        grants,
        invocation_site,
    } = &ctx.auth
    else {
        return Err(api_err(
            ErrorCode::WrongContext,
            "site port unavailable during lifecycle validation",
        ));
    };
    if !ports.authorized(inv, "read:sites") {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "missing read:sites grant",
        ));
    }
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    let bound_site = authority
        .site_id()
        .or_else(|| invocation_site.as_ref().map(|(id, _)| *id));

    let mut sites = ctx.deps.store.list_sites().map_err(map_store_error)?;
    sites.retain(|site| {
        has_read_sites_grant_for(grants, site.id) && bound_site.is_none_or(|b| b == site.id)
    });
    sites.sort_by_key(|a| a.id.to_string());

    let limit = (page.limit.max(1) as usize).min(100);
    let filtered: Vec<SiteView> = sites
        .into_iter()
        .filter(|s| {
            page.cursor
                .as_deref()
                .is_none_or(|cursor| s.id.to_string().as_str() > cursor)
        })
        .take(limit + 1)
        .map(|s| redacted_site_view(&s))
        .collect();

    let has_more = filtered.len() > limit;
    let items: Vec<SiteView> = filtered.into_iter().take(limit).collect();
    let next_cursor = if has_more {
        items.last().map(|s| s.id.clone())
    } else {
        None
    };

    Ok(SitePage { items, next_cursor })
}

pub(super) fn site_get(
    ports: &StoreHostPorts,
    inv: &Invocation,
    handle: &str,
) -> Result<SiteView, ApiError> {
    let obs = load_scoped_site(ports, inv, handle)?;
    Ok(redacted_site_view(obs.site()))
}

const SITE_STATS_CACHE_TTL_SECS: i64 = 300;

pub(super) fn site_stats(
    ports: &StoreHostPorts,
    inv: &Invocation,
    handle: &str,
) -> Result<StatsView, ApiError> {
    let obs = load_scoped_site(ports, inv, handle)?;
    let ctx = ports.lookup(inv)?;
    let site = obs.site();
    let cache_key = (site.id, obs.credential_epoch().storage_key().to_string());
    let now = crate::job_loop::unix_now();
    {
        let cache = ports.site_stats_cache.lock();
        if let Some((cached_at, cached)) = cache.get(&cache_key)
            && now >= *cached_at
            && now - *cached_at < SITE_STATS_CACHE_TTL_SECS
        {
            return Ok(*cached);
        }
    }
    let indexer = ctx
        .deps
        .indexer
        .upgrade()
        .ok_or_else(|| api_err(ErrorCode::Upstream, "indexer unavailable"))?;
    let stats = indexer.fetch_user_stats(site).map_err(|err| match err {
        indexer::IndexerError::UnknownProfile(_)
        | indexer::IndexerError::UnsupportedCapability { .. } => {
            api_err(ErrorCode::Unsupported, &err.to_string())
        }
        indexer::IndexerError::ProviderDisabled { .. } => {
            api_err(ErrorCode::Conflict, &err.to_string())
        }
        indexer::IndexerError::InvalidInput(_) => {
            api_err(ErrorCode::InvalidArgument, &err.to_string())
        }
        other => api_err(ErrorCode::Upstream, &other.to_string()),
    })?;
    let view = match stats {
        Some((u, d)) => StatsView {
            uploaded: Some(u),
            downloaded: Some(d),
        },
        None => StatsView {
            uploaded: None,
            downloaded: None,
        },
    };
    ports.site_stats_cache.lock().insert(cache_key, (now, view));
    Ok(view)
}

pub(super) fn site_credential_epoch(
    ports: &StoreHostPorts,
    handle: &str,
) -> Result<Option<String>, ApiError> {
    let site_id = parse_site_handle(handle)?;
    let Some(deps) = ports.deps.read().clone() else {
        return Err(api_err(
            ErrorCode::Unsupported,
            "site credential epoch unavailable",
        ));
    };
    let obs = deps.store.observe_site(site_id).map_err(map_store_error)?;
    Ok(obs.map(|o| o.credential_epoch().storage_key().to_string()))
}

fn load_scoped_site(
    ports: &StoreHostPorts,
    inv: &Invocation,
    handle: &str,
) -> Result<store::SiteObservation, ApiError> {
    if inv.role == GuestRole::Lifecycle {
        return Err(api_err(
            ErrorCode::WrongContext,
            "site port is forbidden during lifecycle validation",
        ));
    }
    let target_site_id = parse_site_handle(handle)?;
    let ctx = ports.lookup(inv)?;
    let ActiveAuth::Runtime {
        authority,
        grants,
        invocation_site,
    } = &ctx.auth
    else {
        return Err(api_err(
            ErrorCode::WrongContext,
            "site port unavailable during lifecycle validation",
        ));
    };
    if !has_read_sites_grant_for(grants, target_site_id) {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "missing read:sites grant for site",
        ));
    }
    ctx.deps
        .store
        .validate_plugin_state_authority(authority)
        .map_err(map_store_error)?;
    let bound_site = authority
        .site_id()
        .or_else(|| invocation_site.as_ref().map(|(id, _)| *id));
    if let Some(bound) = bound_site
        && bound != target_site_id
    {
        return Err(api_err(
            ErrorCode::PermissionDenied,
            "site access scoped to invocation site",
        ));
    }
    ctx.deps
        .store
        .observe_site(target_site_id)
        .map_err(map_store_error)?
        .ok_or_else(|| api_err(ErrorCode::NotFound, "site not found"))
}

impl StoreHostPorts {
    pub(crate) fn try_resolve_optional_site(
        deps: &HostDeps,
        inv: &Invocation,
    ) -> Result<Option<(SiteId, Option<String>)>, ApiError> {
        let Some(raw_site) = inv.site_id.as_deref() else {
            return Ok(None);
        };
        let Ok(sid) = SiteId::from_str(strip_site_prefix(raw_site)) else {
            return Ok(None);
        };
        if let Some(ep) = inv.credential_epoch.as_deref() {
            let Some(ob) = deps.store.observe_site(sid).map_err(map_store_error)? else {
                return Err(api_err(ErrorCode::StaleGeneration, "site no longer exists"));
            };
            if !ob.site().enabled || ob.credential_epoch().storage_key() != ep {
                return Err(api_err(
                    ErrorCode::StaleGeneration,
                    "site disabled or credential epoch mismatch",
                ));
            }
        }
        Ok(Some((sid, inv.credential_epoch.clone())))
    }

    pub(super) fn resolve_invocation_site(
        deps: &HostDeps,
        inv: &Invocation,
    ) -> Result<(Option<SiteId>, Option<store::SiteObservation>), ApiError> {
        let Some(raw_site) = inv.site_id.as_deref() else {
            return Ok((None, None));
        };
        let sid = SiteId::from_str(strip_site_prefix(raw_site))
            .map_err(|_| api_err(ErrorCode::InvalidArgument, "invalid invocation site_id"))?;
        let obs = deps.store.observe_site(sid).map_err(map_store_error)?;
        if !matches!(
            inv.role,
            GuestRole::Filter | GuestRole::Gate | GuestRole::Notify | GuestRole::Lifecycle
        ) {
            let Some(ref ob) = obs else {
                return Err(api_err(ErrorCode::StaleGeneration, "site no longer exists"));
            };
            if !ob.site().enabled {
                return Err(api_err(
                    ErrorCode::StaleGeneration,
                    "bound site is disabled",
                ));
            }
        }
        if let Some(ep) = inv.credential_epoch.as_deref() {
            let Some(ref ob) = obs else {
                return Err(api_err(ErrorCode::StaleGeneration, "site no longer exists"));
            };
            if !ob.site().enabled || ob.credential_epoch().storage_key() != ep {
                return Err(api_err(
                    ErrorCode::StaleGeneration,
                    "site disabled or credential epoch mismatch",
                ));
            }
        }
        Ok((Some(sid), obs))
    }
}
