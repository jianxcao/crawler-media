//! User-owned Collections: owned SQL facts, then bounded off-lock artwork reads.

mod reads;
mod writes;

pub(crate) use reads::{collection_gallery, collection_items, list_collections};
pub(crate) use writes::{
    add_collection_item, create_collection, delete_collection, remove_collection_item,
    rename_collection, reorder_collection,
};

use axum::http::StatusCode;
use axum::response::Response;
use std::str::FromStr;

use crate::ApiState;
use crate::http::library::{library_for_file, library_visible};
use crate::http::{err, store_wait};

fn store_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "Collection Store operation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

fn ensure_user(store: &crate::Store, user: domain::UserId, token: &str) -> Result<(), Response> {
    match store.user_id_by_token(token).map_err(store_error)? {
        Some(current) if current == user => Ok(()),
        _ => Err(err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        )),
    }
}

fn ensure_collection_owner(
    store: &crate::Store,
    collection_id: &str,
    user_id: domain::UserId,
) -> Result<(), Response> {
    match store
        .collection_owned_by(collection_id, user_id)
        .map_err(store_error)?
    {
        true => Ok(()),
        false => Err(err(
            StatusCode::NOT_FOUND,
            "collection.missing",
            "合集不存在",
        )),
    }
}

struct VisibleCollectionItem {
    media: domain::Media,
    library_id: String,
    coverages: Vec<domain::FileCoverage>,
    is_favorite: bool,
}

fn visible_collection_item(
    store: &crate::Store,
    media_id: domain::MediaId,
    user_id: domain::UserId,
    include_favorite: bool,
) -> Result<Option<VisibleCollectionItem>, Response> {
    let Some(media) = store.get_media(media_id).map_err(store_error)? else {
        return Ok(None);
    };
    let coverages = store
        .file_coverage_for_media(media_id)
        .map_err(store_error)?;
    let Some(library) = coverages
        .iter()
        .filter_map(|coverage| library_for_file(store, &coverage.file, &media))
        .find(|library| library_visible(store, library, Some(user_id)))
    else {
        return Ok(None);
    };
    let coverages = coverages
        .into_iter()
        .filter(|coverage| coverage.file.library_id.to_string() == library.id)
        .collect();
    let is_favorite = if include_favorite {
        crate::http::playback::effective_watch_state(
            store,
            user_id,
            media_id,
            crate::store::UNIT_WHOLE,
            crate::store::UNIT_WHOLE,
        )
        .map_err(store_error)?
        .is_some_and(|unit| unit.favorite)
    } else {
        false
    };
    Ok(Some(VisibleCollectionItem {
        media,
        library_id: library.id,
        coverages,
        is_favorite,
    }))
}

fn visible_collection_items(
    store: &crate::Store,
    collection_id: &str,
    user_id: domain::UserId,
    include_favorite: bool,
) -> Result<Vec<VisibleCollectionItem>, Response> {
    let mut items = Vec::new();
    for raw in store
        .collection_item_ids(collection_id)
        .map_err(store_error)?
    {
        let media = domain::MediaId::from_str(&raw).map_err(store_error)?;
        if let Some(item) = visible_collection_item(store, media, user_id, include_favorite)? {
            items.push(item);
        }
    }
    Ok(items)
}

async fn read<T: Send + 'static>(
    state: &ApiState,
    user: domain::UserId,
    token: String,
    work: impl FnOnce(&crate::Store) -> Result<T, Response> + Send + 'static,
) -> Result<T, Response> {
    store_wait::store_read(state, move |store| {
        ensure_user(store, user, &token)?;
        work(store)
    })
    .await
    .map_err(|error| error.response())?
}

async fn write(
    state: ApiState,
    user: domain::UserId,
    token: String,
    work: impl FnOnce(&crate::Store) -> Response + Send + 'static,
) -> Response {
    store_wait::store_write(&state, move |store| {
        if let Err(response) = ensure_user(store, user, &token) {
            return response;
        }
        work(store)
    })
    .await
    .unwrap_or_else(|response| response)
}
