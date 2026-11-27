use std::collections::BTreeSet;

use domain::{DownloaderId, MediaId, SiteId, UserId};
use serde_json::{Value, json};
use store::{Db, DownloadOperationFilter, SiteMaintenanceReceiptFilter, Store};

use super::service::{map_store_call_error, run_app_read};
use super::types::{PluginServiceError, format_rfc3339};
use super::ui_queries::require_any_ui_cap;
use crate::http::store_wait::store_read;
use crate::management::ApiState;

pub(super) async fn run_db_read<T, F>(
    state: &ApiState,
    dbs: &'static [Db],
    work: F,
) -> Result<T, PluginServiceError>
where
    F: FnOnce(&Store) -> Result<T, store::StoreError> + Send + 'static,
    T: Send + 'static,
{
    Ok(store_read(state, dbs, work)
        .await
        .map_err(map_store_call_error)??)
}

pub(super) async fn query_config_options(
    state: &ApiState,
    caps: &BTreeSet<String>,
    kind: &str,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let items: Vec<Value> = match kind {
        "sites" => {
            require_any_ui_cap(caps, &["read:sites"])?;
            run_app_read(state, move |store| Ok(store.list_sites()?))
                .await?
                .into_iter()
                .take(limit)
                .map(|s| {
                    json!({
                        "id": s.id.to_string(),
                        "name": s.name,
                        "enabled": s.enabled,
                        "capabilities": ["search", "check-in"],
                    })
                })
                .collect()
        }
        "downloaders" => {
            require_any_ui_cap(caps, &["read:downloaders", "read:downloads", "read:boost"])?;
            run_app_read(state, move |store| Ok(store.list_downloaders()?))
                .await?
                .into_iter()
                .take(limit)
                .map(|d| {
                    json!({
                        "id": d.id.to_string(),
                        "name": d.name,
                        "kind": d.kind.to_ascii_lowercase(),
                        "enabled": d.enabled,
                        "is_default": d.is_default,
                        "capabilities": [],
                    })
                })
                .collect()
        }
        "libraries" => {
            require_any_ui_cap(caps, &["read:libraries", "read:library", "read:catalog"])?;
            run_db_read(state, &[Db::Library], |store| store.list_libraries())
                .await?
                .into_iter()
                .take(limit)
                .map(|l| {
                    json!({
                        "id": l.id,
                        "name": l.name,
                        "kind": format!("{:?}", l.kind).to_ascii_lowercase(),
                        "is_default": l.is_default,
                        "capabilities": [],
                    })
                })
                .collect()
        }
        "filters" => {
            require_any_ui_cap(caps, &["read:subscribes", "read:catalog"])?;
            run_app_read(state, move |store| Ok(store.list_filters()?))
                .await?
                .into_iter()
                .take(limit)
                .map(|f| {
                    json!({
                        "id": f.id.to_string(),
                        "name": f.name,
                        "capabilities": [],
                    })
                })
                .collect()
        }
        other => {
            return Err(PluginServiceError::invalid(
                "plugin.invalid_input",
                format!("unsupported config.options kind `{other}`"),
            ));
        }
    };
    Ok(json!({ "items": items, "next_cursor": Value::Null }))
}

pub(super) async fn query_sites_list(
    state: &ApiState,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let sites = run_app_read(state, move |store| Ok(store.list_sites()?)).await?;
    let total = sites.len();
    let items: Vec<Value> = sites
        .into_iter()
        .take(limit)
        .map(|s| {
            json!({
                "id": s.id.to_string(),
                "name": s.name,
                "profile_id": s.profile_id,
                "enabled": s.enabled,
                "maintenance_via": format!("{:?}", s.maintenance_via).to_ascii_lowercase(),
                "timezone": s.timezone,
                "rate_limit_per_minute": s.rate_limit_per_minute,
                "downloader_id": s.downloader_id.map(|d| d.to_string()),
                "has_cookie": s.cookie.as_deref().is_some_and(|c| !c.trim().is_empty()),
                "has_api_key": s.api_key.as_deref().is_some_and(|c| !c.trim().is_empty()),
                "capabilities": ["search", "check-in"],
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total }))
}

pub(super) async fn query_downloaders_list(
    state: &ApiState,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let rows = run_app_read(state, move |store| Ok(store.list_downloaders()?)).await?;
    let total = rows.len();
    let items: Vec<Value> = rows
        .into_iter()
        .take(limit)
        .map(|d| {
            json!({
                "id": d.id.to_string(),
                "name": d.name,
                "kind": d.kind.to_ascii_lowercase(),
                "enabled": d.enabled,
                "is_default": d.is_default,
                "category": d.category,
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total }))
}

pub(super) async fn query_libraries_list(
    state: &ApiState,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let libs = run_db_read(state, &[Db::Library], |store| store.list_libraries()).await?;
    let total = libs.len();
    let items: Vec<Value> = libs
        .into_iter()
        .take(limit)
        .map(|l| {
            json!({
                "id": l.id,
                "name": l.name,
                "kind": format!("{:?}", l.kind).to_ascii_lowercase(),
                "is_default": l.is_default,
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total }))
}

pub(super) async fn query_filters_list(
    state: &ApiState,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let filters = run_app_read(state, move |store| Ok(store.list_filters()?)).await?;
    let total = filters.len();
    let items: Vec<Value> = filters
        .into_iter()
        .take(limit)
        .map(|f| {
            json!({
                "id": f.id.to_string(),
                "name": f.name,
                "atom_count": f.atoms.len(),
                "keep_old_versions": f.keep_old_versions,
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total }))
}

pub(super) async fn query_subscribes_list(
    state: &ApiState,
    user_id: UserId,
    media_id: Option<MediaId>,
    cursor: Option<String>,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let mut subs = run_db_read(state, &[Db::Subscribe], move |store| {
        store.list_subscribes_for_user(user_id)
    })
    .await?;
    if let Some(mid) = media_id {
        subs.retain(|s| s.media_id == mid);
    }
    subs.sort_by_key(|s| s.id.to_string());
    let total = subs.len();
    let cursor_str = cursor.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let filtered: Vec<_> = subs
        .into_iter()
        .filter(|s| cursor_str.is_none_or(|c| s.id.to_string().as_str() > c))
        .collect();
    let has_more = filtered.len() > limit;
    let page_items: Vec<_> = filtered.into_iter().take(limit).collect();
    let next_cursor = if has_more {
        page_items.last().map(|s| Value::String(s.id.to_string()))
    } else {
        None
    };
    let items: Vec<Value> = page_items
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id.to_string(),
                "media_id": s.media_id.to_string(),
                "tracking_state": s.tracking_state,
                "search_interval_secs": s.search_interval_secs,
                "revision": "1",
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total, "next_cursor": next_cursor }))
}

pub(super) async fn query_media_get(
    state: &ApiState,
    media_id: MediaId,
) -> Result<Value, PluginServiceError> {
    let (media, library_id) = run_db_read(state, &[Db::Library], move |store| {
        let Some(media) = store.get_media(media_id)? else {
            return Ok((None, None));
        };
        let coverages = store.file_coverage_for_media(media_id)?;
        let lib_id = coverages
            .iter()
            .find(|fc| fc.file.publication == domain::FilePublication::Published)
            .or_else(|| coverages.first())
            .map(|fc| fc.file.library_id.to_string());
        Ok((Some(media), lib_id))
    })
    .await?;
    let media =
        media.ok_or_else(|| PluginServiceError::not_found("media.missing", "media not found"))?;
    Ok(json!({
        "id": media.id.to_string(),
        "title": media.title,
        "kind": format!("{:?}", media.kind).to_ascii_lowercase(),
        "year": media.year,
        "tmdb_id": media.tmdb_id,
        "douban_id": media.douban_id,
        "library_id": library_id,
        "revision": "1",
    }))
}

pub(super) async fn query_downloads_list(
    state: &ApiState,
    site_id: Option<SiteId>,
    downloader_id: Option<DownloaderId>,
    limit: usize,
    cursor: Option<String>,
) -> Result<Value, PluginServiceError> {
    let page = run_db_read(state, &[Db::Subscribe], move |store| {
        store.list_download_operations(
            &DownloadOperationFilter {
                site_id,
                downloader_id,
                ..DownloadOperationFilter::default()
            },
            cursor.as_deref(),
            Some(limit),
        )
    })
    .await?;
    let items: Vec<Value> = page
        .items
        .into_iter()
        .map(|op| {
            json!({
                "id": op.operation_id,
                "downloader_id": op.downloader_id.map(|d| d.to_string()),
                "site_id": op.site_id.filter(|s| !s.as_uuid().is_nil()).map(|s| s.to_string()),
                "revision": op.revision.to_string(),
                "info_hash": op.info_hash,
                "state": op.state.as_str(),
                "verified_bytes": op.verified_bytes,
                "updated_at": format_rfc3339(op.updated_at),
            })
        })
        .collect();
    Ok(json!({ "items": items, "next_cursor": page.next_cursor }))
}

pub(super) async fn query_checkin_receipts(
    state: &ApiState,
    site_id: Option<SiteId>,
    cursor: Option<String>,
    limit: usize,
) -> Result<Value, PluginServiceError> {
    let page = run_app_read(state, move |store| {
        Ok(store.list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter {
                site_id,
                action: None,
                state: None,
            },
            cursor.as_deref(),
            Some(limit),
        )?)
    })
    .await?;
    let items: Vec<Value> = page
        .items
        .into_iter()
        .map(|r| {
            json!({
                "receipt_id": r.receipt_id.to_string(),
                "site_id": r.site_id.to_string(),
                "action": r.action.as_str(),
                "state": r.state.as_str(),
                "provider_id": r.provider_id,
                "outcome": r.outcome,
                "evidence": r.evidence,
                "updated_at": format_rfc3339(r.updated_at),
            })
        })
        .collect();
    Ok(json!({ "items": items, "next_cursor": page.next_cursor }))
}
