//! Core Admin Plugin HTTP routes & `/plugin-reauth` (`/api/v1`).

mod boost;
mod browser;
mod config_and_perms;
mod dto;
mod endpoints;
mod events;
pub(crate) mod extractors;
mod lifecycle_and_upgrade;
mod multipart;
mod ops_tasks_audit;
mod repositories;
mod trust_and_settings;
mod ui;
mod upload_and_install;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};

use crate::management::ApiState;
use crate::plugins::{OperationAdminView, PagedView, PluginServiceError};
use store::StoreError;
use store::plugins::PluginError;

fn admin_plugin_package_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/plugin-trust-keys",
            get(trust_and_settings::list_trust_keys).post(trust_and_settings::create_trust_key),
        )
        .route(
            "/plugin-trust-keys/{key_id}/revoke",
            post(trust_and_settings::revoke_trust_key),
        )
        .route("/settings/plugins", get(trust_and_settings::get_settings))
        .route(
            "/settings/plugins/developer/enable",
            post(trust_and_settings::enable_developer_mode),
        )
        .route(
            "/settings/plugins/developer/disable",
            post(trust_and_settings::disable_developer_mode),
        )
        .route(
            "/plugin-uploads",
            post(upload_and_install::stage_upload)
                .layer(DefaultBodyLimit::max(multipart::MAX_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/plugin-installations/prepare",
            post(upload_and_install::prepare_install),
        )
        .route(
            "/plugin-installations",
            post(upload_and_install::execute_install),
        )
        .route("/plugins", get(upload_and_install::list_plugins))
        .route(
            "/plugins/dev",
            post(upload_and_install::dev_sideload)
                .layer(DefaultBodyLimit::max(multipart::MAX_UPLOAD_BODY_BYTES)),
        )
        .route("/plugins/{id}", get(upload_and_install::get_plugin))
        .route(
            "/plugins/{id}/config-schema",
            get(config_and_perms::get_config_schema),
        )
        .route(
            "/plugins/{id}/config",
            get(config_and_perms::get_config).put(config_and_perms::update_config),
        )
        .route(
            "/plugins/{id}/permissions",
            get(config_and_perms::get_permissions),
        )
        .route(
            "/plugins/{id}/permissions/grant",
            post(config_and_perms::grant_permissions),
        )
        .route(
            "/plugins/{id}/permissions/revoke",
            post(config_and_perms::revoke_permissions),
        )
        .route(
            "/plugin-hook-catalog",
            get(ops_tasks_audit::get_hook_catalog),
        )
        .route(
            "/plugin-repositories",
            get(repositories::list_repositories).put(repositories::save_repositories),
        )
        .route(
            "/plugin-repositories/{repo_id}/catalog",
            get(repositories::get_catalog),
        )
}

fn admin_plugin_lifecycle_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/plugins/{id}/enable",
            post(lifecycle_and_upgrade::enable_plugin),
        )
        .route(
            "/plugins/{id}/disable",
            post(lifecycle_and_upgrade::disable_plugin),
        )
        .route(
            "/plugins/{id}/uninstall",
            post(lifecycle_and_upgrade::uninstall_plugin),
        )
        .route(
            "/plugins/{id}/upgrades/prepare",
            post(lifecycle_and_upgrade::prepare_upgrade),
        )
        .route(
            "/plugins/{id}/upgrade",
            post(lifecycle_and_upgrade::execute_upgrade),
        )
        .route(
            "/plugins/{id}/rollback",
            post(lifecycle_and_upgrade::rollback_plugin),
        )
        .route(
            "/plugins/{id}/state/reset",
            post(lifecycle_and_upgrade::execute_state_reset),
        )
        .route(
            "/plugin-operations/{id}",
            get(ops_tasks_audit::get_operation),
        )
        .route(
            "/plugin-operations/{id}/cancel",
            post(ops_tasks_audit::cancel_operation),
        )
        .route(
            "/plugin-operations/{id}/resume",
            post(ops_tasks_audit::resume_operation),
        )
        .route("/plugins/{id}/tasks", get(ops_tasks_audit::list_tasks))
        .route(
            "/plugins/{id}/tasks/{task_id}/update",
            post(ops_tasks_audit::update_task),
        )
        .route(
            "/plugins/{id}/tasks/{task_id}/run",
            post(ops_tasks_audit::run_task),
        )
        .route(
            "/plugins/{id}/tasks/{task_id}/receipts",
            get(ops_tasks_audit::list_task_receipts),
        )
        .route(
            "/plugins/{id}/commands",
            post(ops_tasks_audit::submit_command),
        )
        .route("/plugins/{id}/audit", get(ops_tasks_audit::list_audit))
        .route("/plugins/{id}/events", get(events::list_events))
        .route(
            "/plugins/{id}/events/{event_id}/requeue",
            post(events::requeue_event),
        )
        .route("/plugins/{id}/metrics", get(ops_tasks_audit::get_metrics))
        .route(
            "/plugins/{id}/browser-availability",
            get(browser::browser_availability),
        )
        .route(
            "/plugins/{id}/browser-checks",
            post(browser::run_browser_check),
        )
        .route("/plugins/{id}/brush-state", get(boost::brush_state))
        .route(
            "/plugins/{id}/brush-cleanup-preview",
            get(boost::brush_cleanup_preview),
        )
        .route("/plugin-ui-contract", get(ui::plugin_ui_contract))
        .route("/plugin-ui-navigation", get(ui::list_plugin_ui_navigation))
        .route("/plugins/{id}/ui", get(ui::get_plugin_ui))
        .route("/plugins/{id}/ui-trust/grant", post(ui::grant_ui_trust))
        .route("/plugins/{id}/ui-trust/revoke", post(ui::revoke_ui_trust))
        .route(
            "/plugins/{id}/endpoints/{*endpoint_path}",
            axum::routing::any(endpoints::invoke_plugin_endpoint).layer(DefaultBodyLimit::max(
                endpoints::MAX_ENDPOINT_BODY_BYTES + 4096,
            )),
        )
}

pub(crate) fn admin_plugin_asset_routes() -> Router<ApiState> {
    Router::new().route(
        "/plugin-ui-assets/{id}/{bundle_hash}/{*asset_path}",
        get(ui::serve_plugin_ui_asset),
    )
}

/// Write-gated admin plugin routes (`GET` bypasses `write_gate`, mutating
/// `POST`/`PUT` pass through `write_gate`).
pub(crate) fn admin_plugin_routes() -> Router<ApiState> {
    admin_plugin_package_routes().merge(admin_plugin_lifecycle_routes())
}

/// Read-only admin POST routes (no DB mutation, bypasses `write_gate`).
pub(crate) fn admin_plugin_readonly_post_routes() -> Router<ApiState> {
    Router::new()
        .route("/plugin-reauth", post(trust_and_settings::plugin_reauth))
        .route(
            "/plugin-trust-keys/prepare",
            post(trust_and_settings::prepare_trust_key),
        )
        .route(
            "/plugins/{id}/config/validate",
            post(config_and_perms::validate_config),
        )
        .route(
            "/plugins/{id}/permissions/prepare",
            post(config_and_perms::prepare_permissions),
        )
        .route(
            "/plugins/{id}/state/reset/prepare",
            post(lifecycle_and_upgrade::prepare_state_reset),
        )
        .route("/plugins/{id}/ui-trust/prepare", post(ui::prepare_ui_trust))
        .route("/plugins/{id}/ui/query", post(ui::execute_ui_query))
}

pub(super) fn ok_json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_value(value) {
        Ok(data) => (status, axum::Json(json!({ "ok": true, "data": data }))).into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to serialize plugin response");
            crate::http::err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin.internal_error",
                "内部服务错误",
            )
        }
    }
}

pub(crate) fn ok_with_etag<T: Serialize>(status: StatusCode, revision: i64, value: &T) -> Response {
    let mut resp = ok_json(status, value);
    attach_etag(resp.headers_mut(), revision);
    resp
}

pub(super) fn created_with_etag_location<T: Serialize>(
    revision: i64,
    location: &str,
    value: &T,
) -> Response {
    let mut resp = ok_with_etag(StatusCode::CREATED, revision, value);
    if let Ok(val) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, val);
    }
    resp
}

pub(super) fn ok_paged<T: Serialize>(page: PagedView<T>) -> Response {
    match serde_json::to_value(&page.items) {
        Ok(items) => (
            StatusCode::OK,
            axum::Json(json!({
                "ok": true,
                "data": items,
                "next_cursor": page.next_cursor,
            })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to serialize paged plugin response");
            crate::http::err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin.internal_error",
                "内部服务错误",
            )
        }
    }
}

pub(crate) fn accepted_op(op: OperationAdminView) -> Response {
    let loc = format!("/api/v1/plugin-operations/{}", op.operation_id);
    let mut resp = ok_with_etag(StatusCode::ACCEPTED, op.revision, &op);
    if let Ok(val) = HeaderValue::from_str(&loc) {
        resp.headers_mut().insert(header::LOCATION, val);
    }
    resp
}

pub(super) fn attach_etag(headers: &mut HeaderMap, revision: i64) {
    if let Ok(val) = HeaderValue::from_str(&format!("\"{revision}\"")) {
        headers.insert(header::ETAG, val);
    }
}

pub(crate) fn service_error_response(err: PluginServiceError) -> Response {
    let (status_u16, code) = err.status_and_code();
    let status = StatusCode::from_u16(status_u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(code, error = %err, "plugin HTTP request failed");
        return crate::http::err(status, code, "内部服务错误");
    }

    let mut error_obj = json!({
        "code": code,
        "message": err.to_string(),
    });
    if !err.fields().is_empty()
        && let Ok(fields_json) = serde_json::to_value(err.fields())
    {
        error_obj["fields"] = fields_json;
    }
    if let PluginServiceError::Store(StoreError::Plugin(PluginError::OperationActive(op_id))) = &err
    {
        error_obj["operation_id"] = Value::String(op_id.to_string());
    }

    let mut resp = (
        status,
        axum::Json(json!({
            "ok": false,
            "error": error_obj,
        })),
    )
        .into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    resp
}
