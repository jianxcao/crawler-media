use crate::{
    ApiState, Store,
    http::{err, ok, store_wait},
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::{FileCoverage, LibraryId, Media, MediaId, MediaKind};
use serde_json::{Value, json};
use std::path::Path as FsPath;

mod candidates;

pub(crate) async fn preview(
    State(state): State<ApiState>,
    Path((library, item)): Path<(String, String)>,
) -> Response {
    let (Ok(library), Ok(media)) = (library.parse::<LibraryId>(), item.parse::<MediaId>()) else {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid",
            "条目或Library id无效",
        );
    };
    let (media, coverages, revision) =
        match store_wait::store_read(&state, &[store::Db::Library], move |store| {
            capture(store, library, media)
        })
        .await
        {
            Ok(Ok(value)) => value,
            Ok(Err(response)) => return response,
            Err(error) => return error.response(),
        };
    let files = state.file_stats.clone();
    let selected = crate::http::library::preferred_coverage(&coverages);
    let selected_file_id = selected.map(|coverage| coverage.file.file_id);
    let poster_file = selected.map(|coverage| coverage.file.clone());
    let has_poster = match crate::http::fs::read_work(&state, move || {
        poster_file.and_then(|file| crate::http::library::poster_path_with(&file, files.as_ref()))
    })
    .await
    {
        Ok(poster) => poster.is_some(),
        Err(error) => return crate::http::library::browse_work_error(error),
    };
    let poster_url = if has_poster {
        selected_file_id.map(|file_id| crate::http::library::artwork_url("posters", file_id))
    } else {
        None
    };
    let catalog = state.catalog.clone();
    // `capture` returned owned facts and released its reader snapshot. Catalog
    // and filesystem IO below hold no Store reader or writer lease.
    match state
        .http_work
        .run(crate::http::library::BROWSE_DEADLINE, move || {
            render(media, coverages, revision, poster_url, catalog.as_ref())
        })
        .await
    {
        Ok(response) => response,
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

fn capture(
    store: &Store,
    library: LibraryId,
    media: MediaId,
) -> Result<(Media, Vec<FileCoverage>, i64), Response> {
    let media = store
        .get_media(media)
        .map_err(store_error)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
    let coverages = store
        .file_coverage_in_library(library, Some(media.id), store::FileReadScope::Published)
        .map_err(store_error)?;
    let revision = store
        .media_revision(media.id)
        .map_err(store_error)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
    Ok((media, coverages, revision))
}
fn store_error(error: store::StoreError) -> Response {
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

fn render(
    media: Media,
    coverages: Vec<FileCoverage>,
    source_revision: i64,
    poster_url: Option<String>,
    catalog: &dyn crate::catalog::Catalog,
) -> Response {
    let search_seed = coverages
        .iter()
        .filter_map(|coverage| {
            FsPath::new(&coverage.file.path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| release::parse(name).title)
        })
        .next()
        .unwrap_or_else(|| media.title.clone());
    let (outcome, unreachable) = if coverages.is_empty() {
        (Value::Null, false)
    } else {
        candidates::outcome(&media, &search_seed, poster_url.clone(), catalog)
    };
    let groups: Vec<Value> = if coverages.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "key": media.id.to_string(),
            "outcome": outcome,
            "files": coverages.iter().map(|coverage| json!({
                "file_id": coverage.file.file_id.to_string(),
                "revision": coverage.file.version.revision,
                "source_version": coverage.file.version.source_version,
            })).collect::<Vec<_>>(),
            "file_count": coverages.len(),
            "total_size_bytes": 0,
            "sample_names": coverages.iter().take(3).filter_map(|coverage| {
                FsPath::new(&coverage.file.path).file_name().and_then(|n| n.to_str()).map(str::to_string)
            }).collect::<Vec<_>>(),
        })]
    };
    ok(json!({
        "current": super::review_item(&media, poster_url),
        "movie": media.kind == MediaKind::Movie,
        "groups": groups,
        "skipped_missing": 0,
        "pinned_identity": false,
        "unreachable": unreachable,
        "search_seed": search_seed,
        "media_item_id": media.id.to_string(),
        "source_revision": source_revision,
    }))
    .into_response()
}
