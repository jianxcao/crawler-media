use super::*;

use crate::http::ok;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};

fn name(body: &Value) -> Result<String, Response> {
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        Err(err(
            StatusCode::BAD_REQUEST,
            "collection.invalid",
            "合集名不能为空",
        ))
    } else {
        Ok(name)
    }
}

pub(crate) async fn create_collection(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = match name(&body) {
        Ok(name) => name,
        Err(response) => return response,
    };
    write(state, user, token, move |store| {
        match store.create_collection(user, &name) {
            Ok(id) => {
                tracing::info!(collection_id=%id, user_id=%user, "Collection created");
                ok(json!({"id":id})).into_response()
            }
            Err(error) => store_error(error),
        }
    })
    .await
}

pub(crate) async fn rename_collection(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = match name(&body) {
        Ok(name) => name,
        Err(response) => return response,
    };
    write(state, user, token, move |store| {
        if let Err(response) = ensure_collection_owner(store, &id, user) {
            return response;
        }
        match store.rename_collection(&id, &name) {
            Ok(true) => {
                tracing::info!(collection_id=%id, user_id=%user, "Collection renamed");
                ok(json!({"renamed":true})).into_response()
            }
            Ok(false) => err(StatusCode::NOT_FOUND, "collection.missing", "合集不存在"),
            Err(error) => store_error(error),
        }
    })
    .await
}

pub(crate) async fn delete_collection(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
) -> Response {
    write(state, user, token, move |store| {
        if let Err(response) = ensure_collection_owner(store, &id, user) {
            return response;
        }
        match store.delete_collection(&id) {
            Ok(true) => {
                tracing::info!(collection_id=%id, user_id=%user, "Collection deleted");
                ok(json!({"deleted":true})).into_response()
            }
            Ok(false) => err(StatusCode::NOT_FOUND, "collection.missing", "合集不存在"),
            Err(error) => store_error(error),
        }
    })
    .await
}

pub(crate) async fn add_collection_item(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some(media) = body["media_item_id"]
        .as_str()
        .and_then(|id| id.parse::<domain::MediaId>().ok())
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "collection.invalid",
            "media_item_id 无效",
        );
    };
    write(state, user, token, move |store| {
        if let Err(response) = ensure_collection_owner(store, &id, user) {
            return response;
        }
        match visible_collection_item(store, media, user, false) {
            Ok(Some(_)) => {}
            Ok(None) => return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"),
            Err(response) => return response,
        }
        match store.add_collection_item(&id, &media.to_string()) {
            Ok(added) => {
                tracing::info!(collection_id=%id, media_id=%media, added, "Collection item added");
                ok(json!({"added":added})).into_response()
            }
            Err(error) => store_error(error),
        }
    })
    .await
}

pub(crate) async fn remove_collection_item(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path((id, media)): Path<(String, String)>,
) -> Response {
    write(state, user, token, move |store| {
        if let Err(response) = ensure_collection_owner(store, &id, user) { return response; }
        match store.remove_collection_item(&id, &media) {
            Ok(removed) => { tracing::info!(collection_id=%id, media_id=%media, removed, "Collection item removed");
                ok(json!({"removed":removed})).into_response() },
            Err(error) => store_error(error),
        }
    }).await
}

pub(crate) async fn reorder_collection(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body["media_item_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    write(state, user, token, move |store| {
        if let Err(response) = ensure_collection_owner(store, &id, user) {
            return response;
        }
        match store.reorder_collection_items(&id, &ids) {
            Ok(_) => {
                tracing::info!(collection_id=%id, "Collection reordered");
                ok(json!({"reordered":true})).into_response()
            }
            Err(error) => store_error(error),
        }
    })
    .await
}
