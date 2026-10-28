use crate::http::store_wait::{StoreCallError, store_read, store_write};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
mod aggregate;
use aggregate::aggregate_media_entries;

use crate::management::{ApiError, ApiState};
use crate::store::CatalogCacheRow;

#[derive(serde::Serialize, Clone)]
pub struct MediaCacheEntry {
    pub id: String,
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub kind: String, // "movie" | "tv"
    pub year: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub overview: Option<String>,
    pub rating: Option<f64>,
    pub genres: Vec<String>,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
    pub cached_keys: Vec<String>,
}

#[derive(Deserialize)]
pub struct ListCatalogQuery {
    pub view: Option<String>,
}

pub async fn list_catalog_cache(
    State(state): State<ApiState>,
    Query(query): Query<ListCatalogQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = store_read(&state, |store| store.list_catalog_cache())
        .await
        .map_err(cache_store_error)??;
    let aggregate = query.view.as_deref() == Some("aggregate");
    state
        .http_work
        .run(std::time::Duration::from_secs(12), move || {
            catalog_cache_view(rows, aggregate)
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Catalog cache rendering worker unavailable");
            ApiError::with(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请稍后重试".into(),
            )
        })
}

fn catalog_cache_view(rows: Vec<CatalogCacheRow>, aggregate: bool) -> Json<Value> {
    if aggregate {
        let items = aggregate_media_entries(rows);
        let list_val: Vec<Value> = items
            .into_iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "source": item.source,
                    "source_id": item.source_id,
                    "title": item.title,
                    "original_title": item.original_title,
                    "kind": item.kind,
                    "year": item.year,
                    "poster_url": item.poster_url,
                    "backdrop_url": item.backdrop_url,
                    "overview": item.overview,
                    "rating": item.rating,
                    "genres": item.genres,
                    "fetched_at": item.fetched_at,
                    "expires_at": item.expires_at,
                    "cached_keys": item.cached_keys,
                })
            })
            .collect();

        return Json(json!({ "ok": true, "data": list_val }));
    }

    let raw_val: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let val: Option<Value> = serde_json::from_str(row.body.trim()).ok();
            let title = val
                .as_ref()
                .and_then(|v| {
                    v.get("title")
                        .or_else(|| v.get("name"))
                        .and_then(Value::as_str)
                        .or_else(|| v.pointer("/results/0/title").and_then(Value::as_str))
                        .or_else(|| v.pointer("/results/0/name").and_then(Value::as_str))
                })
                .unwrap_or("-");
            json!({
                "source": row.source,
                "cache_key": row.cache_key,
                "fetched_at": row.fetched_at,
                "expires_at": row.expires_at,
                "title": title,
            })
        })
        .collect();
    Json(json!({ "ok": true, "data": raw_val }))
}

#[derive(Deserialize)]
pub struct DeleteMediaCacheQuery {
    source: String,
    cache_key: Option<String>,
}

pub async fn delete_catalog_cache(
    State(state): State<ApiState>,
    Query(query): Query<DeleteMediaCacheQuery>,
) -> Result<Json<Value>, ApiError> {
    let key = query
        .cache_key
        .filter(|key| !key.is_empty())
        .ok_or_else(|| ApiError::invalid("catalog.invalid", "cache_key is required".into()))?;
    let deleted = store_write(&state, move |store| {
        // An unknown source is a client error, not a server failure.
        match store.delete_catalog_cache(&query.source, &key) {
            Err(store::StoreError::Invalid(message)) => {
                return Err(ApiError::invalid("catalog.invalid", message));
            }
            other => other.map_err(ApiError::from),
        }
    })
    .await
    .map_err(cache_admission_error)??;
    Ok(Json(json!({ "ok": true, "data": { "deleted": deleted } })))
}

fn cache_store_error(error: StoreCallError) -> ApiError {
    match error {
        StoreCallError::Busy | StoreCallError::Deadline => ApiError::with(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请稍后重试".into(),
        ),
        StoreCallError::Worker(message) => {
            tracing::error!(%message, "Catalog cache Store worker failed");
            ApiError::with(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "读取失败".into(),
            )
        }
    }
}
fn cache_admission_error(response: axum::response::Response) -> ApiError {
    match response.status() {
        StatusCode::CONFLICT => ApiError::with(
            StatusCode::CONFLICT,
            "instance.read_only",
            "实例为只读，拒绝写入".into(),
        ),
        StatusCode::SERVICE_UNAVAILABLE => ApiError::with(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请稍后重试".into(),
        ),
        _ => ApiError::with(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            "读取失败".into(),
        ),
    }
}
