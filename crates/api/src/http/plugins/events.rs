use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use store::StoreError;
use store::plugin_events::{OutboxDb, OutboxDeliveryDetail, OutboxDeliveryStatus};
use store::plugins::PluginError;

use super::extractors::{parse_query_pairs, validate_limit};
use super::{ok_json, service_error_response};
use crate::http::err;
use crate::http::store_wait::{store_read, store_write};
use crate::management::ApiState;
use crate::plugins::PluginServiceError;
use crate::plugins::events::{OUTBOX_DBS, unix_now_ms_fallback, wake};

const fn outbox_store_dbs(db: OutboxDb) -> &'static [store::Db] {
    match db {
        OutboxDb::App => &[store::Db::App],
        OutboxDb::Subscribe => &[store::Db::Subscribe],
        OutboxDb::Library => &[store::Db::Library],
    }
}

#[derive(Serialize)]
struct PluginEventListResponse {
    items: Vec<PluginEventDeliveryView>,
}

#[derive(Serialize)]
struct PluginEventDeliveryView {
    event_id: String,
    db: &'static str,
    plugin_id: String,
    event_domain: String,
    event_kind: String,
    entity_type: String,
    entity_id: String,
    entity_sequence: i64,
    entity_revision: i64,
    cause_id: String,
    status: &'static str,
    attempt_count: i64,
    next_attempt_at: i64,
    claimed_at: Option<i64>,
    lease_until: Option<i64>,
    last_error: Option<String>,
    delivered_at: Option<i64>,
    dead_lettered_at: Option<i64>,
    revision: i64,
    occurred_at: i64,
    created_at: i64,
    updated_at: i64,
}

impl PluginEventDeliveryView {
    fn from_detail(db: OutboxDb, detail: OutboxDeliveryDetail) -> Self {
        let d = detail.delivery;
        let e = detail.event;
        Self {
            event_id: d.event_id,
            db: db.as_str(),
            plugin_id: d.plugin_id,
            event_domain: e.event_domain,
            event_kind: e.event_kind,
            entity_type: d.entity_type,
            entity_id: d.entity_id,
            entity_sequence: d.entity_sequence,
            entity_revision: e.entity_revision,
            cause_id: e.cause_id,
            status: d.status.as_str(),
            attempt_count: d.attempt_count,
            next_attempt_at: d.next_attempt_at,
            claimed_at: d.claimed_at,
            lease_until: d.lease_until,
            last_error: d.last_error,
            delivered_at: d.delivered_at,
            dead_lettered_at: d.dead_lettered_at,
            revision: d.revision,
            occurred_at: e.occurred_at,
            created_at: d.created_at,
            updated_at: d.updated_at,
        }
    }
}

pub(super) async fn list_events(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let mut status: Option<OutboxDeliveryStatus> = None;
    let mut limit: Option<usize> = None;
    for (k, v) in parse_query_pairs(raw_query.as_deref()) {
        match k.as_str() {
            "status" => match v.parse::<OutboxDeliveryStatus>() {
                Ok(parsed) => status = Some(parsed),
                Err(()) => {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "plugin.invalid_input",
                        "unsupported event delivery status",
                    );
                }
            },
            "limit" => match validate_limit(&v) {
                Ok(l) => limit = Some(l),
                Err(resp) => return resp,
            },
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "plugin.invalid_input",
                    &format!("unsupported query parameter: {k}"),
                );
            }
        }
    }

    let plugin_id = id.clone();
    let plugin_exists = match store_read(&state, &[store::Db::App], move |store| {
        store.get_plugin(&plugin_id)
    })
    .await
    {
        Ok(Ok(Some(_))) => true,
        Ok(Ok(None)) => false,
        Ok(Err(e)) => return service_error_response(PluginServiceError::from(e)),
        Err(e) => return e.response(),
    };
    if !plugin_exists {
        return err(
            StatusCode::NOT_FOUND,
            "plugin.not_found",
            "plugin not found",
        );
    }

    let cap = limit.unwrap_or(100).clamp(1, 200);
    let mut combined: Vec<PluginEventDeliveryView> = Vec::new();
    for db in OUTBOX_DBS {
        let pid = id.clone();
        let res = match store_read(&state, outbox_store_dbs(db), move |store| {
            store.list_plugin_event_deliveries(db, &pid, status, None, Some(cap))
        })
        .await
        {
            Ok(Ok(page)) => page,
            Ok(Err(e)) => return service_error_response(PluginServiceError::from(e)),
            Err(e) => return e.response(),
        };
        for item in res.items {
            combined.push(PluginEventDeliveryView::from_detail(db, item));
        }
    }
    combined.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    combined.truncate(cap);
    ok_json(StatusCode::OK, &PluginEventListResponse { items: combined })
}

#[derive(Serialize)]
struct RequeueResponse {
    event_id: String,
    db: &'static str,
    plugin_id: String,
    status: &'static str,
    attempt_count: i64,
    next_attempt_at: i64,
    revision: i64,
}

pub(super) async fn requeue_event(
    State(state): State<ApiState>,
    Path((id, event_id)): Path<(String, String)>,
) -> Response {
    let pid_check = id.clone();
    let exists = match store_read(&state, &[store::Db::App], move |store| {
        store.get_plugin(&pid_check)
    })
    .await
    {
        Ok(Ok(Some(_))) => true,
        Ok(Ok(None)) => false,
        Ok(Err(e)) => return service_error_response(PluginServiceError::from(e)),
        Err(e) => return e.response(),
    };
    if !exists {
        return err(
            StatusCode::NOT_FOUND,
            "plugin.not_found",
            "plugin not found",
        );
    }

    let mut target_db: Option<OutboxDb> = None;
    for db in OUTBOX_DBS {
        let pid = id.clone();
        let eid = event_id.clone();
        let found = match store_read(&state, outbox_store_dbs(db), move |store| {
            store.get_delivery_receipt(db, &eid, &pid)
        })
        .await
        {
            Ok(Ok(Some(_))) => true,
            Ok(Ok(None)) => false,
            Ok(Err(e)) => return service_error_response(PluginServiceError::from(e)),
            Err(e) => return e.response(),
        };
        if found {
            target_db = Some(db);
            break;
        }
    }
    let Some(db) = target_db else {
        return err(
            StatusCode::NOT_FOUND,
            "plugin.event_not_found",
            "plugin event delivery not found",
        );
    };

    let now_ms = unix_now_ms_fallback();
    let pid = id.clone();
    let eid = event_id.clone();
    let updated = match store_write(&state, move |store| {
        store.requeue_dead_letter(db, &eid, &pid, now_ms, now_ms)
    })
    .await
    {
        Ok(Ok(rec)) => rec,
        Ok(Err(StoreError::Plugin(PluginError::NotFound))) => {
            return err(
                StatusCode::NOT_FOUND,
                "plugin.event_not_found",
                "plugin event delivery not found",
            );
        }
        Ok(Err(StoreError::Plugin(PluginError::Conflict))) => {
            return err(
                StatusCode::CONFLICT,
                "plugin.conflict",
                "only dead_letter event deliveries can be requeued",
            );
        }
        Ok(Err(e)) => return service_error_response(PluginServiceError::from(e)),
        Err(resp) => return resp,
    };

    wake(&state);
    ok_json(
        StatusCode::OK,
        &RequeueResponse {
            event_id: updated.event_id,
            db: db.as_str(),
            plugin_id: updated.plugin_id,
            status: updated.status.as_str(),
            attempt_count: updated.attempt_count,
            next_attempt_at: updated.next_attempt_at,
            revision: updated.revision,
        },
    )
}
