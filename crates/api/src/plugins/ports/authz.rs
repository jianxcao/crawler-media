use domain::SiteId;
use store::plugins::{PluginGrantScope, PluginStateAuthority};

use super::ActiveAuth;

pub(super) fn is_capability_authorized(auth: &ActiveAuth, cap: &str) -> bool {
    if matches!(cap, "config" | "log" | "runtime" | "clock") {
        return true;
    }
    match auth {
        ActiveAuth::Lifecycle { .. } => false,
        ActiveAuth::Runtime {
            authority,
            grants,
            invocation_site,
        } => {
            if cap == "operations" {
                return true;
            }
            let supported = matches!(
                cap,
                "kv" | "blob"
                    | "http"
                    | "http:site-auth"
                    | "read:sites"
                    | "read:boost"
                    | "browser:search"
                    | "browser:maintenance"
            ) || cap.starts_with("command:");
            if !supported {
                return false;
            }
            let bound_site = bound_site_id(authority, invocation_site.as_ref());
            if cap == "read:sites" && bound_site.is_none() {
                return grants.iter().any(|g| {
                    g.capability == "read:sites"
                        && match &g.scope {
                            PluginGrantScope::Global | PluginGrantScope::Site(_) => true,
                            PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
                                super::parse_site_scope_id(id).is_some()
                            }
                            _ => false,
                        }
                });
            }
            if cap == "read:boost" {
                return grants.iter().any(|g| {
                    g.capability == "read:boost" && boost_scope_matches(&g.scope, bound_site)
                });
            }
            if cap.starts_with("command:") {
                return grants.iter().any(|g| {
                    command_capability_matches(cap, &g.capability)
                        && command_scope_matches(cap, &g.scope, bound_site)
                });
            }
            grants
                .iter()
                .any(|g| g.capability == cap && grant_matches_site(&g.scope, bound_site))
        }
    }
}

fn bound_site_id(
    authority: &PluginStateAuthority,
    invocation_site: Option<&(SiteId, Option<String>)>,
) -> Option<SiteId> {
    authority
        .site_id()
        .or_else(|| invocation_site.map(|(id, _)| *id))
}

fn boost_scope_matches(scope: &PluginGrantScope, bound_site: Option<SiteId>) -> bool {
    match scope {
        PluginGrantScope::Global => true,
        PluginGrantScope::Site(id) => bound_site.is_none_or(|b| b == *id),
        PluginGrantScope::Object { kind, id } => match kind.trim() {
            "pool" => true,
            "site" => super::parse_site_scope_id(id)
                .is_some_and(|sid| bound_site.is_none_or(|b| b == sid)),
            "downloader" => super::parse_downloader_scope_id(id).is_some(),
            _ => false,
        },
    }
}

pub(super) fn command_capability_matches(requested_cap: &str, granted_cap: &str) -> bool {
    granted_cap == requested_cap
        || (matches!(
            requested_cap,
            "command:site.check-in" | "command:site.verify-session"
        ) && granted_cap == "command:site.maintenance")
}

fn command_scope_matches(
    requested_cap: &str,
    scope: &PluginGrantScope,
    bound_site: Option<SiteId>,
) -> bool {
    match scope {
        PluginGrantScope::Global => true,
        PluginGrantScope::Site(s) => bound_site.is_none_or(|b| b == *s),
        PluginGrantScope::Object { kind, id } => match kind.trim() {
            "site"
                if requested_cap.starts_with("command:site.")
                    || matches!(
                        requested_cap,
                        "command:indexer.search" | "command:indexer.rss"
                    ) =>
            {
                super::parse_site_scope_id(id)
                    .is_some_and(|sid| bound_site.is_none_or(|b| b == sid))
            }
            "downloader" if requested_cap.starts_with("command:downloader.") => {
                super::parse_downloader_scope_id(id).is_some()
            }
            _ => false,
        },
    }
}

fn grant_matches_site(scope: &PluginGrantScope, target: Option<SiteId>) -> bool {
    match scope {
        PluginGrantScope::Global => true,
        PluginGrantScope::Site(id) => target == Some(*id),
        PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
            target.is_some() && super::parse_site_scope_id(id) == target
        }
        _ => false,
    }
}

pub(crate) fn ensure_provider_role_grant(
    role: plugin_api::GuestRole,
    grants: &[store::plugins::PluginGrant],
    site_id: Option<SiteId>,
) -> Result<(), plugin_api::ApiError> {
    let required_cap = match role {
        plugin_api::GuestRole::Indexer => "indexer:provide",
        plugin_api::GuestRole::Maintenance => "maintenance:provide",
        _ => return Ok(()),
    };
    let covered = grants.iter().any(|g| {
        g.capability == required_cap
            && match &g.scope {
                PluginGrantScope::Global => true,
                PluginGrantScope::Site(expected) => site_id == Some(*expected),
                PluginGrantScope::Object { kind, id } if kind.trim() == "site" => {
                    site_id.is_some() && super::parse_site_scope_id(id) == site_id
                }
                _ => false,
            }
    });
    if covered {
        Ok(())
    } else {
        Err(super::api_err(
            plugin_api::ErrorCode::PermissionDenied,
            &format!("missing {required_cap} grant for invocation site"),
        ))
    }
}
