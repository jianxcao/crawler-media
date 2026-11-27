use std::str::FromStr;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use domain::{SiteId, UserId};
use indexer::MaintenanceAction;
use serde::Deserialize;
use serde_json::json;
use store::SiteMaintenanceReceiptFilter;
use store::plugins::{PluginLifecycle, PluginRecord};

use crate::http::err;
use crate::http::plugins::extractors::CanonicalWrite;
use crate::http::plugins::{accepted_op, ok_with_etag, service_error_response};
use crate::http::store_wait::store_read;
use crate::management::ApiState;
use crate::plugins::builtins::BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID;
use crate::plugins::{SubmitAdminCommandInput, op_settlement};

const OFFICIAL_CHECKIN_PLUGIN_ID: &str = "cm.checkin";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SitePluginActionRequest {
    pub action: String,
    #[serde(default)]
    pub provider_id: Option<String>,
}

fn normalize_action(raw: &str) -> Result<(&'static str, MaintenanceAction), Response> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "login" => Ok(("login", MaintenanceAction::VerifySession)),
        "verify" | "verify-session" => Ok(("verify-session", MaintenanceAction::VerifySession)),
        "checkin" | "check-in" => Ok(("check-in", MaintenanceAction::CheckIn)),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "site.invalid_action",
            "action must be `login`, `checkin`, or `verify`",
        )),
    }
}

pub(crate) async fn plugin_status(
    State(state): State<ApiState>,
    Path(raw_id): Path<String>,
) -> Response {
    let id = match SiteId::from_str(&raw_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let read = match store_read(&state, &[store::Db::App], move |store| {
        let Some(obs) = store.observe_site(id)? else {
            return Ok::<_, store::StoreError>(None);
        };
        let receipts = store.list_site_maintenance_receipts(
            &SiteMaintenanceReceiptFilter {
                site_id: Some(id),
                action: None,
                state: None,
            },
            None,
            Some(20),
        )?;
        let idx_plugin =
            if let Ok((pid, _)) = indexer::parse_plugin_provider_id(&obs.site().profile_id) {
                store.get_plugin(pid)?
            } else {
                None
            };
        Ok(Some((obs, receipts.items, idx_plugin)))
    })
    .await
    {
        Ok(Ok(Some(triple))) => triple,
        Ok(Ok(None)) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
        Ok(Err(e)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "site.store_error",
                &e.to_string(),
            );
        }
        Err(error) => return error.response(),
    };

    let (observed, receipts, idx_plugin) = read;
    let site = observed.site();
    let (available, indexer_provider, check_in_provider) =
        resolve_site_plugin_providers(&state, site, idx_plugin.as_ref());

    let cdp_enabled = state.browser.cdp_enabled().unwrap_or(false);
    let receipt_views: Vec<_> = receipts.into_iter().map(receipt_view_json).collect();

    let payload = json!({
        "site_id": site.id.to_string(),
        "revision": observed.revision(),
        "credential_epoch": observed.credential_epoch().storage_key(),
        "profile_id": site.profile_id,
        "timezone": site.timezone,
        "enabled": site.enabled,
        "available": available,
        "maintenance_via": site.maintenance_via.as_str(),
        "browser_search": site.browser_search,
        "browser": {
            "cdp_enabled": cdp_enabled,
            "endpoint_configured": site.cdp_url.as_deref().is_some_and(|u| !u.trim().is_empty()) || cdp_enabled,
        },
        "indexer_provider": indexer_provider,
        "maintenance_provider": check_in_provider,
        "receipts": receipt_views,
    });
    ok_with_etag(StatusCode::OK, observed.revision(), &payload)
}

fn resolve_site_plugin_providers(
    state: &ApiState,
    site: &domain::Site,
    idx_plugin: Option<&store::plugins::PluginRecord>,
) -> (bool, Option<serde_json::Value>, Option<serde_json::Value>) {
    let check_in_provider = crate::site_maintenance::resolve_maintenance_provider(
        state,
        site,
        MaintenanceAction::CheckIn,
        None,
    )
    .or_else(|_| {
        crate::site_maintenance::resolve_maintenance_provider(
            state,
            site,
            MaintenanceAction::VerifySession,
            None,
        )
    })
    .ok()
    .map(|(plugin_id, generation, prov)| {
        let desc = prov.descriptor();
        json!({
            "plugin_id": plugin_id,
            "provider_id": desc.id,
            "generation": generation,
            "actions": desc.actions.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            "availability": match &desc.availability {
                indexer::ProviderAvailability::Enabled => "enabled",
                indexer::ProviderAvailability::Disabled { .. } => "disabled",
            },
        })
    });
    let reg = state.indexer.snapshot();
    let (available, indexer_provider) =
        if let Ok((pid, _)) = indexer::parse_plugin_provider_id(&site.profile_id) {
            let plugin_enabled =
                idx_plugin.is_some_and(|p| p.lifecycle == store::plugins::PluginLifecycle::Enabled);
            let reg_enabled = reg.indexer(&site.profile_id).is_some_and(|entry| {
                matches!(
                    entry.provider().descriptor().availability,
                    indexer::ProviderAvailability::Enabled
                )
            });
            let is_avail = plugin_enabled && reg_enabled;
            (
                is_avail,
                Some(json!({
                    "plugin_id": pid,
                    "provider_id": site.profile_id,
                    "availability": if is_avail { "enabled" } else { "disabled" },
                })),
            )
        } else {
            (site.enabled, None)
        };
    (available, indexer_provider, check_in_provider)
}

fn receipt_view_json(r: store::SiteMaintenanceReceipt) -> serde_json::Value {
    json!({
        "receipt_id": r.receipt_id,
        "action": r.action.as_str(),
        "window_key": r.window_key,
        "timezone": r.timezone,
        "plugin_id": r.plugin_id,
        "provider_id": r.provider_id,
        "state": r.state.as_str(),
        "outcome": r.outcome,
        "revision": r.revision,
        "updated_at": r.updated_at,
    })
}

async fn check_site_action_replay(
    state: &ApiState,
    user_id: UserId,
    w: &CanonicalWrite<SitePluginActionRequest>,
) -> Result<Option<crate::plugins::OperationAdminView>, crate::plugins::PluginServiceError> {
    for candidate in [
        OFFICIAL_CHECKIN_PLUGIN_ID,
        BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID,
    ] {
        if let Some(op) = op_settlement::check_management_replay(
            state,
            user_id,
            Some(candidate),
            &w.idempotency_key,
            &w.request,
            "site.maintenance",
        )
        .await?
        {
            return Ok(Some(op));
        }
    }
    Ok(None)
}

pub(crate) async fn plugin_actions(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(raw_id): Path<String>,
    w: CanonicalWrite<SitePluginActionRequest>,
) -> Response {
    let id = match SiteId::from_str(&raw_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let (normalized_action, maint_action) = match normalize_action(&w.body.action) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };

    match check_site_action_replay(&state, user_id, &w).await {
        Ok(Some(replayed)) => return accepted_op(replayed),
        Ok(None) => {}
        Err(err) => return service_error_response(err),
    }

    let read = match store_read(&state, &[store::Db::App], move |store| {
        let obs = store.observe_site(id)?;
        let cm_checkin = store.get_plugin(OFFICIAL_CHECKIN_PLUGIN_ID)?;
        let maint = store.get_plugin(BUILTIN_NEXUSPHP_MAINTENANCE_PLUGIN_ID)?;
        Ok::<_, store::StoreError>((obs, cm_checkin, maint))
    })
    .await
    {
        Ok(Ok(triple)) => triple,
        Ok(Err(e)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "site.store_error",
                &e.to_string(),
            );
        }
        Err(error) => return error.response(),
    };
    let (Some(observed), cm_checkin, maint) = read else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    if observed.revision() != w.expected_revision {
        return err(
            StatusCode::CONFLICT,
            "plugin.conflict",
            "site revision conflict",
        );
    }
    let api_check_in_supported = maint_action == MaintenanceAction::CheckIn
        && w.body.provider_id.is_none()
        && state
            .indexer
            .snapshot()
            .profile(&observed.site().profile_id)
            .is_some_and(|p| crate::check_in::uses_api_check_in(&p, observed.site()));
    if !api_check_in_supported
        && let Err(error) = crate::site_maintenance::resolve_maintenance_provider(
            &state,
            observed.site(),
            maint_action,
            w.body.provider_id.as_deref(),
        )
    {
        return err(
            StatusCode::BAD_REQUEST,
            "site.maintenance_unsupported",
            &error.to_string(),
        );
    }
    let Some(plugin) = select_enabled_plugin(cm_checkin, maint) else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "plugin.missing",
            "site maintenance plugin unavailable",
        );
    };

    match state
        .plugins()
        .submit_admin_command(
            &state,
            &plugin.plugin_id,
            SubmitAdminCommandInput {
                actor_user_id: user_id,
                kind: "site.maintenance".into(),
                payload: json!({
                    "site_id": id.to_string(),
                    "action": normalized_action,
                    "provider_id": w.body.provider_id,
                    "expected_site_revision": observed.revision(),
                    "expected_credential_epoch": observed.credential_epoch().storage_key(),
                }),
                targets: Vec::new(),
                evidence_ref: Some(format!("site-plugin-action:{id}:{normalized_action}")),
                expected_revision: plugin.revision,
                idempotency_key: w.idempotency_key,
                request: w.request,
            },
        )
        .await
    {
        Ok(op) => accepted_op(op),
        Err(err) => service_error_response(err),
    }
}

fn select_enabled_plugin(
    preferred: Option<PluginRecord>,
    fallback: Option<PluginRecord>,
) -> Option<PluginRecord> {
    preferred
        .filter(|p| p.lifecycle == PluginLifecycle::Enabled)
        .or_else(|| fallback.filter(|p| p.lifecycle == PluginLifecycle::Enabled))
}
