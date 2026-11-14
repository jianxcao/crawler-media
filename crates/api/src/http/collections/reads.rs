use super::*;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::file_stat_read::FileStatRead;
use crate::http::library::{BROWSE_DEADLINE, poster_path_with, preferred_coverage};
use crate::http::{ok, ok_list};

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
        let allowed = store.authorized_library_ids(user).map_err(store_error)?;
        if libraries.into_iter().any(|id| !allowed.contains(&id)) {
            return Err(err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目已不可见",
            ));
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
        let libraries = authorized_libraries(store, user)?;
        store
            .collection_summaries(user, &libraries)
            .map_err(store_error)
    })
    .await
    {
        Ok(facts) => facts,
        Err(response) => return response,
    };
    let libraries = facts
        .iter()
        .flat_map(|row| row.items.iter().map(|item| item.library_id.to_string()))
        .collect();
    let files = state.file_stats.clone();
    let items = match observe(&state, move || {
        facts.into_iter().map(|row| json!({
        "id": row.collection.id, "name": row.collection.name, "item_count": row.items.len(),
        "cover_url": row.items.iter().find_map(|item| poster_url(&item.coverages, files.as_ref())),
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
) -> Result<Vec<store::CollectionItemFacts>, Response> {
    read(state, user, token, move |store| {
        ensure_collection_owner(store, &id, user)?;
        let libraries = authorized_libraries(store, user)?;
        store
            .collection_items_all(user, &id, &libraries)
            .map_err(store_error)
    })
    .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionPageQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

fn authorized_libraries(
    store: &crate::Store,
    user: domain::UserId,
) -> Result<Vec<domain::LibraryId>, Response> {
    store
        .authorized_library_ids(user)
        .map_err(store_error)?
        .into_iter()
        .map(|id| id.parse().map_err(store_error))
        .collect()
}

pub(crate) async fn collection_items(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
    Query(query): Query<CollectionPageQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(40);
    if !(1..=200).contains(&limit) {
        return err(
            StatusCode::BAD_REQUEST,
            "collection.invalid",
            "limit 必须为 1–200",
        );
    }
    let collection = id.clone();
    let page = match read(&state, user, token.clone(), move |store| {
        ensure_collection_owner(store, &collection, user)?;
        let libraries = authorized_libraries(store, user)?;
        store
            .collection_items_page(
                user,
                &collection,
                &libraries,
                limit,
                query.cursor.as_deref(),
            )
            .map_err(|error| match error {
                store::StoreError::Invalid(message) => {
                    err(StatusCode::BAD_REQUEST, "collection.invalid", &message)
                }
                error => store_error(error),
            })
    })
    .await
    {
        Ok(page) => page,
        Err(response) => return response,
    };
    let libraries = page
        .items
        .iter()
        .map(|item| item.library_id.to_string())
        .collect();
    let files = state.file_stats.clone();
    let items = match observe(&state, move || {
        page.items.into_iter().map(|item| json!({
            "media_item_id": item.media.id.to_string(), "library_id": item.library_id.to_string(),
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
    ok(json!({"items":items,"total":page.total,"next_cursor":page.next_cursor})).into_response()
}

fn gallery_item(item: store::CollectionItemFacts, files: &dyn FileStatRead) -> Value {
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
    json!({"media_item_id":item.media.id.to_string(), "library_id":item.library_id.to_string(),
        "kind":item.media.kind.as_str(), "title":item.media.title, "year":item.media.year,
        "is_favorite":item.is_favorite, "images":images})
}

pub(crate) async fn collection_gallery(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    axum::Extension(token): axum::Extension<String>,
    Path(id): Path<String>,
) -> Response {
    let facts = match capture(&state, user, token.clone(), id.clone()).await {
        Ok(facts) => facts,
        Err(response) => return response,
    };
    let libraries = facts
        .iter()
        .map(|item| item.library_id.to_string())
        .collect();
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
