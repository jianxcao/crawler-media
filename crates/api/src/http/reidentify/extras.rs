use super::*;
use store::library_write::{WriteScope, demote::FileDemotion};

pub(crate) async fn extras(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let blocking = state.clone();
    crate::http::subscriptions::guarded_blocking(&blocking, move || detach(&state, body)).await
}

fn detach(state: &ApiState, body: Value) -> Response {
    let ids: HashSet<_> = body["file_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .filter_map(|raw| raw.parse::<LedgerId>().ok())
        .collect();
    if ids.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少有效 file_ids",
        );
    }
    let captured = super::super::store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        ids.iter()
            .filter_map(|id| match store.file_coverage_by_id(*id) {
                Ok(Some(row)) => Some(
                    store
                        .library_config_revision(&row.file.library_id.to_string())
                        .map(|revision| FileDemotion {
                            scope: WriteScope {
                                library_id: row.file.library_id,
                                config_revision: revision,
                            },
                            file: row.file,
                        })
                        .map_err(store_error),
                ),
                Ok(None) => None,
                Err(error) => Some(Err(store_error(error))),
            })
            .collect::<Result<Vec<_>, Response>>()
    });
    let files = match captured {
        Some(Ok(files)) => files,
        Some(Err(response)) => return response,
        None => return super::super::store_wait::busy_response(),
    };
    // Never hold Store while acquiring Registry -> Store leases.
    let ids: Vec<_> = files.iter().map(|request| request.file.file_id).collect();
    let _leases = match acquire_file_leases(state, &ids) {
        Ok(leases) => leases,
        Err(response) => return response,
    };
    match super::super::store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        store
            .demote_files_to_unidentified(&files)
            .map_err(store_error)
    }) {
        Some(Ok(detached)) => ok(json!({"detached":detached})).into_response(),
        Some(Err(response)) => response,
        None => super::super::store_wait::busy_response(),
    }
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "Published file demotion rejected");
    let status = if matches!(
        error,
        store::StoreError::Protected(_) | store::StoreError::Missing(_)
    ) {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    err(status, "reidentify.detach_failed", &error.to_string())
}
