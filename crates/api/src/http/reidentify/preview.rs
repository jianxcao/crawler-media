use crate::{
    ApiState, Store,
    http::{err, library::preferred_coverage, ok, store_wait},
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::{FileCoverage, LibraryId, Media, MediaId, MediaKind};
use serde_json::{Value, json};
use std::path::Path as FsPath;

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
        match store_wait::store_read(&state, move |store| capture(store, library, media)).await {
            Ok(Ok(value)) => value,
            Ok(Err(response)) => return response,
            Err(error) => return error.response(),
        };
    let files = state.file_stats.clone();
    match crate::http::fs::read_work(&state, move || {
        render(media, coverages, revision, files.as_ref())
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
    files: &dyn crate::file_stat_read::FileStatRead,
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
    let selected = preferred_coverage(&coverages);
    let poster =
        selected.and_then(|coverage| crate::http::library::poster_path_with(&coverage.file, files));
    let poster_url = selected
        .filter(|_| poster.is_some())
        .map(|coverage| crate::http::library::artwork_url("posters", coverage.file.file_id));
    let groups: Vec<Value> = if coverages.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "key": media.id.to_string(),
            "outcome": {
                "media_item_id": media.id.to_string(),
                "tmdb_id": media.tmdb_id,
                "title": media.title,
                "year": media.year,
                "poster_url": poster_url.clone(),
                "source": "current",
                "same_as_current": true,
                "reason": null,
                "code": null,
                "candidates": [],
            },
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
        "unreachable": false,
        "search_seed": search_seed,
        "media_item_id": media.id.to_string(),
        "source_revision": source_revision,
    }))
    .into_response()
}
