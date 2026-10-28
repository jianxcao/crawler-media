//! Metadata refresh owns database snapshots before observing files or providers.
use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{FileCoverage, Media, MediaId};
use serde_json::json;

use crate::http::{err, ok, store_wait};
use crate::library_lifecycle::{BoundedLeaseError, LibraryWorkLease};
use crate::{ApiState, Store};

pub(crate) async fn refresh_metadata(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let worker = state.clone();
    match state
        .fs_work
        .run_to_completion(move || refresh(&worker, &id))
        .await
    {
        Ok(response) => response,
        Err(error) => crate::http::library::browse_work_error(error),
    }
}

struct Captured {
    groups: HashMap<MediaId, Vec<FileCoverage>>,
    media: HashMap<MediaId, Media>,
}

fn capture(store: &Store, id: &str) -> Result<Captured, Response> {
    let library = store
        .get_library(id)
        .map_err(store_error)?
        .ok_or_else(crate::http::library::missing_library)?;
    let library_id = library
        .id
        .parse()
        .map_err(|_| crate::http::library::missing_library())?;
    let files = store
        .file_coverage_in_library(library_id, None, store::FileReadScope::Published)
        .map_err(store_error)?;
    let ids: HashSet<_> = files.iter().map(|row| row.file.media_id).collect();
    let media = store
        .media_batch(&ids.into_iter().collect::<Vec<_>>())
        .map_err(store_error)?;
    let mut groups = HashMap::<_, Vec<_>>::new();
    for coverage in files {
        groups
            .entry(coverage.file.media_id)
            .or_default()
            .push(coverage);
    }
    Ok(Captured { groups, media })
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "捕获 Library 元数据刷新事实失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

fn lease_error(error: BoundedLeaseError) -> Response {
    match error {
        BoundedLeaseError::RegistryBusy | BoundedLeaseError::StoreBusy => {
            store_wait::busy_response()
        }
        other => err(StatusCode::CONFLICT, "library.deleting", &other.to_string()),
    }
}

fn refresh(state: &ApiState, id: &str) -> Response {
    let lease = match state
        .library_work
        .acquire_bounded(state.store().as_ref(), id)
    {
        Ok(lease) => lease,
        Err(error) => return lease_error(error),
    };
    let captured = match store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        capture(store, id)
    }) {
        Some(Ok(captured)) => captured,
        Some(Err(response)) => return response,
        None => return store_wait::busy_response(),
    };
    tracing::info!(
        library_id = id,
        media_count = captured.groups.len(),
        "Library 元数据刷新开始"
    );
    let mut refreshed = 0u32;
    for (media_id, coverages) in captured.groups {
        let Some(media) = captured.media.get(&media_id) else {
            continue;
        };
        // One observation per file, with no Store guard. NAS latency stays on this worker.
        let live: Vec<_> = coverages
            .iter()
            .filter(|row| {
                state
                    .file_stats
                    .metadata(&row.file.path)
                    .is_ok_and(|meta| meta.is_file())
            })
            .collect();
        let Some(coverage) = crate::http::library::preferred_coverage(&live) else {
            continue;
        };
        if let Err(response) = check_work(state, id, &lease) {
            return response;
        }
        let path = &coverage.file.path;
        if crate::poster_fetch::attach_poster(state, media, path).is_some() {
            refreshed += 1;
        }
        let _ = crate::poster_fetch::force_attach_backdrop(state, media, path);
        super::scrape_library_metadata(state, media, coverage, path, &coverages);
        if let Err(response) = check_work(state, id, &lease) {
            return response;
        }
        crate::http::library_chapters::auto_generate_chapters(state, path);
    }
    tracing::info!(library_id = id, refreshed, "Library 元数据刷新完成");
    ok(json!({"queued":true,"refreshed":refreshed})).into_response()
}

fn check_work(state: &ApiState, id: &str, lease: &LibraryWorkLease) -> Result<(), Response> {
    store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        if lease.control().is_cancelled() || !store.library_accepts_work(id).map_err(store_error)? {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除或已冻结",
            ));
        }
        Ok(())
    })
    .unwrap_or_else(|| Err(store_wait::busy_response()))
}
