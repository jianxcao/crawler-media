use domain::{DownloaderId, SiteId, UserId};
use serde_json::Value;
use store::plugins::{PluginCommandTarget, PluginGrantScope, PluginRecord};

use super::types::PluginServiceError;
use crate::management::ApiState;

pub(crate) fn targets_cover_downloader(
    targets: &[PluginCommandTarget],
    dl_id: DownloaderId,
) -> bool {
    targets.iter().any(|t| {
        matches!(t, PluginCommandTarget::Global)
            || matches!(t, PluginCommandTarget::Downloader(id) if *id == dl_id)
    })
}

pub(crate) fn targets_cover_site(targets: &[PluginCommandTarget], site_id: SiteId) -> bool {
    targets.iter().any(|t| {
        matches!(t, PluginCommandTarget::Global)
            || matches!(t, PluginCommandTarget::Site(id) if *id == site_id)
    })
}

pub(crate) fn grant_scopes_cover_downloader(
    scopes: &[PluginGrantScope],
    dl_id: DownloaderId,
) -> bool {
    scopes.iter().any(|s| match s {
        PluginGrantScope::Global => true,
        PluginGrantScope::Object { kind, id } => {
            kind.trim() == "downloader"
                && super::ports::parse_downloader_scope_id(id) == Some(dl_id)
        }
        _ => false,
    })
}

pub(crate) fn downloader_scope_ids(
    scopes: &[PluginGrantScope],
) -> impl Iterator<Item = DownloaderId> + '_ {
    scopes.iter().filter_map(|s| match s {
        PluginGrantScope::Object { kind, id } if kind.trim() == "downloader" => {
            super::ports::parse_downloader_scope_id(id)
        }
        _ => None,
    })
}

pub(crate) fn grant_scopes_cover_site(scopes: &[PluginGrantScope], site_id: SiteId) -> bool {
    scopes.iter().any(|s| match s {
        PluginGrantScope::Global => true,
        PluginGrantScope::Site(id) => *id == site_id,
        PluginGrantScope::Object { kind, id } => {
            kind.trim() == "site" && super::ports::parse_site_scope_id(id) == Some(site_id)
        }
    })
}

pub(crate) fn plugin_grants_cover_download_site(
    store: &store::Store,
    plugin_id: &str,
    site_id: SiteId,
) -> Result<bool, PluginServiceError> {
    const SITE_CAPS: &[&str] = &[
        "read:sites",
        "read:boost",
        "http:site-auth",
        "indexer:provide",
        "maintenance:provide",
        "browser:search",
        "browser:maintenance",
        "command:indexer.search",
        "command:indexer.rss",
        "command:site.maintenance",
        "command:site.check-in",
        "command:site.verify-session",
    ];
    let grants = store.get_plugin_grants(plugin_id)?;
    Ok(grants.iter().any(|g| {
        SITE_CAPS.contains(&g.capability.as_str())
            && grant_scopes_cover_site(std::slice::from_ref(&g.scope), site_id)
    }))
}

pub(crate) fn validate_admin_command_for_targets(
    state: &ApiState,
    actor_user_id: UserId,
    plugin: &PluginRecord,
    kind: &str,
    targets: &[PluginCommandTarget],
    payload: &Value,
) -> Result<(), PluginServiceError> {
    match kind {
        "downloader.control"
        | "downloader.delete-task"
        | "downloader.prepare-cleanup"
        | "downloader.delete-files" => {
            crate::download_service::plugin_command_validate::validate_admin_command(
                state,
                actor_user_id,
                plugin,
                kind,
                targets,
                payload,
            )
        }
        "downloader.add" => crate::download_service::plugin_command::validate_admin_downloader_add(
            &state.store,
            &plugin.plugin_id,
            targets,
            payload,
        ),
        "site.maintenance" | "site.check-in" | "site.verify-session" => {
            let site_id = crate::site_maintenance::parse_payload_site_id(payload)?;
            if !targets_cover_site(targets, site_id) {
                return Err(PluginServiceError::forbidden(
                    "plugin.forbidden",
                    "command targets do not cover payload site_id",
                ));
            }
            Ok(())
        }
        "boost.run" => crate::plugins::boost::cycle::validate_admin_boost_run_targets(
            state, plugin, targets, payload,
        ),
        "indexer.search" => super::ui_indexer::validate_indexer_search_command(
            state,
            actor_user_id,
            plugin,
            targets,
            payload,
        ),
        _ => Ok(()),
    }
}
