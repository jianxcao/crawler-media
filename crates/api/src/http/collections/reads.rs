use super::*;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::file_stat_read::FileStatRead;
use crate::http::library::{BROWSE_DEADLINE, poster_path_with, preferred_coverage};
use crate::http::ok_list;

fn poster_url(coverages: &[domain::FileCoverage], files: &dyn FileStatRead) -> Option<String> {
    let coverage = preferred_coverage(coverages)?;
    poster_path_with(&coverage.file, files)?;
    Some(crate::http::library::artwork_url(
        "posters",
        coverage.file.file_id,
    ))
}

async fn observe(
    state: &ApiState,
    work: impl FnOnce() -> Vec<Value> + Send + 'static,
) -> Result<Vec<Value>, Response> {
    state
        .http_work
        .run(BROWSE_DEADLINE, work)
        .await
        .map_err(|error| {
            tracing::error!(%error, "Collection artwork observation failed");
            crate::http::library::browse_work_error(error)
        })
}

/// Recheck owner, credential and Library visibility after potentially slow disk IO.
async fn validate(
    state: &ApiState,
    user: domain::UserId,
    token: String,
    collection: Option<String>,
    libraries: Vec<String>,
) -> Result<(), Response> {
    read(state, user, token, move |store| {
        if let Some(collection) = collection {
            ensure_collection_owner(store, &collection, user)?;
        }
        for id in libraries
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
        {
            if !store
                .get_library(&id)
                .map_err(store_error)?
                .is_some_and(|library| library_visible(store, &library, Some(user)))
            {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "条目已不可见",
                ));
            }
        }
        Ok(())
    })
    .await
}

pub(crate) async fn list_collections(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
) -> Response {
    let facts = match read(&state, user, token.clone(), move |store| {
        store
            .list_collections(user)
            .map_err(store_error)?
            .into_iter()
            .map(|collection| {
                let items = visible_collection_items(store, &collection.id, user, false)?;
                Ok((collection, items))
            })
            .collect::<Result<Vec<_>, Response>>()
    })
    .await
    {
        Ok(facts) => facts,
        Err(response) => return response,
    };
    let libraries = facts
        .iter()
        .flat_map(|(_, items)| items.iter().map(|item| item.library_id.clone()))
        .collect();
    let files = state.file_stats.clone();
    let items = match observe(&state, move || {
        facts.into_iter().map(|(collection, items)| json!({
        "id": collection.id, "name": collection.name, "item_count": items.len(),
        "cover_url": items.iter().find_map(|item| poster_url(&item.coverages, files.as_ref())),
    })).collect()
    })
    .await
    {
        Ok(items) => items,
        Err(response) => return response,
    };
    if let Err(response) = validate(&state, user, token, None, libraries).await {
        return response;
    }
    ok_list(items).into_response()
}

async fn capture(
    state: &ApiState,
    user: domain::UserId,
    token: String,
    id: String,
    include_favorite: bool,
) -> Result<Vec<VisibleCollectionItem>, Response> {
    read(state, user, token, move |store| {
        ensure_collection_owner(store, &id, user)?;
        visible_collection_items(store, &id, user, include_favorite)
    })
    .await
}

pub(crate) async fn collection_items(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
) -> Response {
    let facts = match capture(&state, user, token.clone(), id.clone(), false).await {
        Ok(facts) => facts,
        Err(response) => return response,
    };
    let libraries = facts.iter().map(|item| item.library_id.clone()).collect();
    let files = state.file_stats.clone();
    let items = match observe(&state, move || {
        facts.into_iter().map(|item| json!({
        "media_item_id": item.media.id.to_string(), "library_id": item.library_id,
        "kind": item.media.kind.as_str(), "title": item.media.title, "year": item.media.year,
        "poster_url": poster_url(&item.coverages, files.as_ref()),
    })).collect()
    })
    .await
    {
        Ok(items) => items,
        Err(response) => return response,
    };
    if let Err(response) = validate(&state, user, token, Some(id), libraries).await {
        return response;
    }
    ok_list(items).into_response()
}

fn gallery_item(item: VisibleCollectionItem, files: &dyn FileStatRead) -> Value {
    let mut images = Vec::new();
    if let Some(coverage) = preferred_coverage(&item.coverages)
        && let Some(dir) = coverage.file.path.parent()
    {
        for (path, kind, endpoint, aspect, label) in [
            ("poster.jpg", "poster", "posters", 0.667, "海报"),
            ("fanart.jpg", "backdrop", "fanart", 1.778, "背景"),
        ] {
            if files.is_file(&dir.join(path)) {
                images.push(json!({"kind":kind,
                    "url":crate::http::library::artwork_url(endpoint, coverage.file.file_id),
                    "aspect":aspect, "label":label, "season":null, "episode":null, "t_seconds":null}));
            }
        }
    }
    json!({"media_item_id":item.media.id.to_string(), "library_id":item.library_id,
        "kind":item.media.kind.as_str(), "title":item.media.title, "year":item.media.year,
        "is_favorite":item.is_favorite, "images":images})
}

pub(crate) async fn collection_gallery(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
) -> Response {
    let facts = match capture(&state, user, token.clone(), id.clone(), true).await {
        Ok(facts) => facts,
        Err(response) => return response,
    };
    let libraries = facts.iter().map(|item| item.library_id.clone()).collect();
    let files = state.file_stats.clone();
    let groups = match observe(&state, move || {
        facts
            .into_iter()
            .map(|item| gallery_item(item, files.as_ref()))
            .collect()
    })
    .await
    {
        Ok(groups) => groups,
        Err(response) => return response,
    };
    if let Err(response) = validate(&state, user, token, Some(id), libraries).await {
        return response;
    }
    ok_list(groups).into_response()
}
