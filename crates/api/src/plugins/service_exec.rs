use store::Store;

use super::types::PluginServiceError;
use crate::http::store_wait::{StoreCallError, store_read, with_foreground_store};
use crate::management::ApiState;

pub(super) fn require_foreground_write_admitted(
    state: &ApiState,
) -> Result<(), PluginServiceError> {
    state.require_writable().map_err(map_admission_response)?;
    state
        .require_write_admitted()
        .map_err(map_admission_response)?;
    Ok(())
}

pub(super) fn run_foreground_store_sync<T, F>(
    state: &ApiState,
    work: F,
) -> Result<T, PluginServiceError>
where
    F: FnOnce(&Store) -> Result<T, PluginServiceError>,
{
    with_foreground_store(state, work)
        .map_err(map_admission_response)?
        .map_err(|err| match err {
            PluginServiceError::Store(
                store::StoreError::WriteBusy
                | store::StoreError::ReadBusy
                | store::StoreError::ReadDeadline,
            ) => PluginServiceError::Busy {
                code: "runtime.busy",
                message: "store lock busy".into(),
            },
            other => other,
        })
}

pub(super) fn run_foreground_app_tx<T, F>(
    state: &ApiState,
    foreground: bool,
    work: F,
) -> Result<T, PluginServiceError>
where
    F: FnOnce(&store::App<'_>) -> Result<T, PluginServiceError>,
{
    if foreground {
        let inner = with_foreground_store(state, |store| {
            super::config_ops::app_write_service(store, work)
        })
        .map_err(map_admission_response)?;
        inner.map_err(|err| match err {
            PluginServiceError::Store(
                store::StoreError::WriteBusy
                | store::StoreError::ReadBusy
                | store::StoreError::ReadDeadline,
            ) => PluginServiceError::Busy {
                code: "runtime.busy",
                message: "store lock busy".into(),
            },
            other => other,
        })
    } else {
        super::config_ops::app_write_service(&state.store, work)
    }
}

pub(crate) async fn run_app_read<T, F>(state: &ApiState, work: F) -> Result<T, PluginServiceError>
where
    F: FnOnce(&Store) -> Result<T, PluginServiceError> + Send + 'static,
    T: Send + 'static,
{
    store_read(state, &[store::Db::App], work)
        .await
        .map_err(map_store_call_error)?
}

pub(crate) async fn run_app_write<T, F>(state: &ApiState, work: F) -> Result<T, PluginServiceError>
where
    F: FnOnce(&Store) -> Result<T, PluginServiceError> + Send + 'static,
    T: Send + 'static,
{
    let worker = state.clone();
    let res = state
        .http_work
        .run_to_completion(move || {
            with_foreground_store(&worker, work).map_err(map_admission_response)
        })
        .await;
    match res {
        Ok(Ok(inner)) => inner,
        Ok(Err(err)) => Err(err),
        Err(crate::blocking_budget::BlockingError::Busy) => Err(PluginServiceError::Busy {
            code: "runtime.busy",
            message: "foreground write queue busy".into(),
        }),
        Err(other) => Err(PluginServiceError::Io(other.to_string())),
    }
}

pub(crate) async fn run_blocking<T, F>(work: F) -> Result<T, PluginServiceError>
where
    F: FnOnce() -> Result<T, PluginServiceError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| PluginServiceError::Io(e.to_string()))?
}

pub(crate) fn map_store_call_error(err: StoreCallError) -> PluginServiceError {
    match err {
        StoreCallError::Busy | StoreCallError::Deadline => PluginServiceError::Busy {
            code: "runtime.busy",
            message: "store lock busy".into(),
        },
        StoreCallError::Worker(msg) => PluginServiceError::Io(msg),
    }
}

fn map_admission_response(resp: axum::response::Response) -> PluginServiceError {
    if resp.status() == axum::http::StatusCode::CONFLICT {
        PluginServiceError::Conflict {
            code: "instance.read_only",
            message: "instance is read-only".into(),
        }
    } else {
        PluginServiceError::Busy {
            code: "runtime.busy",
            message: "maintenance write gate closed".into(),
        }
    }
}
