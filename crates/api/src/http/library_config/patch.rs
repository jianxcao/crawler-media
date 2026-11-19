use super::*;
use crate::storage_operations::{self, ReconfigurationRequest};
use store::library_operations::LibraryOperationView;

pub(super) struct Applied {
    pub library: store::Library,
    pub operation: Option<Value>,
}

pub(super) fn apply(
    state: &ApiState,
    id: &str,
    body: &Value,
    request: ReconfigurationRequest,
) -> Result<Applied, StorageOperationError> {
    let kind = if request.root_paths.is_some() {
        "roots"
    } else {
        "locale"
    };
    let prepared = if request.root_paths.is_some() || request.display_locale.is_some() {
        Some(storage_operations::prepare_library_reconfiguration(
            state, id, request,
        )?)
    } else {
        None
    };
    let now_ms = state.playback_clock.now_ms();
    let committed = state
        .library_work
        .configure_settings(state, id, |store| {
            let settings = |db: &store::LibraryConnection<'_>| apply_library_settings(db, id, body);
            let ((library, disabling), status) = match &prepared {
                Some(prepared) => {
                    let (value, status) =
                        store.reconfigure_library_with(prepared, now_ms, settings)?;
                    (value, Some(status))
                }
                None => (store.library_write(settings)?, None),
            };
            let operation = status.map(|status| LibraryOperationView {
                operation_id: status.operation_id,
                library_id: id.into(),
                kind: kind.into(),
                state: status.state,
                cursor: None,
                error: status.error,
                updated_at_ms: now_ms,
            });
            let operation_id = operation.as_ref().map(|op| op.operation_id.clone());
            Ok(((library, operation), disabling, operation_id))
        })
        .map_err(StorageOperationError::Admission)?;
    let (library, operation) = committed.map_err(|error| patch_error(state, id, error))?;
    if let Some(operation) = &operation {
        storage_operations::spawn_driver(state, &operation.operation_id, id);
        tracing::info!(library_id = id, operation_id = %operation.operation_id, kind, "Library 重配置已受理");
    }
    Ok(Applied {
        library,
        operation: operation.as_ref().map(storage_operations::operation_json),
    })
}

fn patch_error(state: &ApiState, id: &str, error: StoreError) -> StorageOperationError {
    if !matches!(error, StoreError::Protected(_)) {
        return storage_operations::map_store_error(error);
    }
    match super::super::store_wait::with_store_read(state, &[store::Db::Library], |store| {
        store.library_accepts_work(id)
    }) {
        Ok(Ok(false)) => StorageOperationError::Store(error),
        Ok(Ok(true)) => storage_operations::reconfiguration_error(
            state,
            id,
            StorageOperationError::Store(error),
        ),
        Ok(Err(error)) => storage_operations::map_store_error(error),
        Err(response) => StorageOperationError::Admission(response),
    }
}
