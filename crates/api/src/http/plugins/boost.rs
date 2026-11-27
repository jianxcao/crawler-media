use std::str::FromStr;

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use domain::{DownloaderId, SiteId, UserId};

use super::extractors::parse_query_pairs;
use super::{ok_json, service_error_response};
use crate::http::err;
use crate::management::ApiState;
use crate::plugins::boost::{CleanupPreviewQuery, get_brush_state_http, preview_brush_cleanup};

fn invalid_input(message: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "plugin.invalid_input", message)
}

fn parse_optional_id<T: FromStr>(
    slot: &mut Option<T>,
    name: &str,
    raw: &str,
) -> Result<(), Response> {
    if slot.is_some() {
        return Err(invalid_input(&format!("duplicate {name} query param")));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_input(&format!("{name} must not be empty")));
    }
    let id = trimmed
        .parse::<T>()
        .map_err(|_| invalid_input(&format!("invalid {name} UUID")))?;
    *slot = Some(id);
    Ok(())
}

pub(super) async fn brush_state(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Response {
    if crate::plugins::has_attribution_headers(&headers)
        && let Err(error) = state
            .plugins()
            .verify_plugin_attribution_with_cap(&state, &plugin_id, &headers, Some("read:boost"))
            .await
    {
        return service_error_response(error);
    }
    let mut site_id: Option<SiteId> = None;
    let mut downloader_id: Option<DownloaderId> = None;
    for (key, value) in parse_query_pairs(raw.as_deref()) {
        match key.as_str() {
            "site_id" => {
                if let Err(resp) = parse_optional_id(&mut site_id, "site_id", &value) {
                    return resp;
                }
            }
            "downloader_id" => {
                if let Err(resp) = parse_optional_id(&mut downloader_id, "downloader_id", &value) {
                    return resp;
                }
            }
            other => {
                return invalid_input(&format!("unknown query parameter `{other}`"));
            }
        }
    }

    match get_brush_state_http(&state, &plugin_id, site_id, downloader_id).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(error) => service_error_response(error),
    }
}

pub(super) async fn brush_cleanup_preview(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Response {
    if let Err(error) = state
        .plugins()
        .verify_plugin_attribution_with_cap(
            &state,
            &plugin_id,
            &headers,
            Some("command:downloader.delete-files"),
        )
        .await
    {
        return service_error_response(error);
    }
    let mut task_ids_seen = false;
    let mut task_ids: Vec<String> = Vec::new();
    let mut plan_handle: Option<String> = None;

    for (key, value) in parse_query_pairs(raw.as_deref()) {
        match key.as_str() {
            "task_ids" | "task_ids[]" => {
                task_ids_seen = true;
                for part in value.split(',') {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() && !task_ids.iter().any(|existing| existing == trimmed) {
                        task_ids.push(trimmed.to_string());
                    }
                }
            }
            "plan_handle" => {
                if plan_handle.is_some() {
                    return invalid_input("duplicate plan_handle query param");
                }
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return invalid_input("plan_handle must not be empty");
                }
                plan_handle = Some(trimmed.to_string());
            }
            other => {
                return invalid_input(&format!("unknown query parameter `{other}`"));
            }
        }
    }

    let query = match (task_ids_seen, plan_handle) {
        (true, None) if !task_ids.is_empty() => CleanupPreviewQuery::Selection { task_ids },
        (false, Some(handle)) => CleanupPreviewQuery::Plan {
            plan_handle: handle,
        },
        (true, Some(_)) => {
            return invalid_input("specify either task_ids or plan_handle, not both");
        }
        _ => {
            return invalid_input("either non-empty task_ids or plan_handle is required");
        }
    };

    match preview_brush_cleanup(&state, user_id, &plugin_id, query).await {
        Ok(view) => ok_json(StatusCode::OK, &view),
        Err(error) => service_error_response(error),
    }
}
