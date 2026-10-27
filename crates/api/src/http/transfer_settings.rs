//! Instance settings only; roots and Naming templates use their own contracts.
use crate::{
    http::{err, ok, strict_json::StrictJson},
    management::ApiState,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferInput {
    expected_revision: i64,
    #[serde(default, deserialize_with = "present_value")]
    transfer_mode: Option<String>,
    #[serde(default, deserialize_with = "present_value")]
    scrape: Option<bool>,
    #[serde(default, deserialize_with = "nullable_path")]
    watch_intake: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_path")]
    watch_inplace: Option<Option<String>>,
}

fn present_value<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn nullable_path<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

pub(crate) async fn get(State(state): State<ApiState>) -> Response {
    match super::store_wait::store_read(&state, |store| store.transfer_settings()).await {
        Ok(Ok(settings)) => ok(serde_json::json!(settings)).into_response(),
        Ok(Err(error)) => failure(error),
        Err(error) => error.response(),
    }
}

pub(crate) async fn patch(
    State(state): State<ApiState>,
    StrictJson(body): StrictJson<TransferInput>,
) -> Response {
    let patch = store::TransferSettingsPatch {
        transfer_mode: body.transfer_mode,
        scrape: body.scrape,
        watch_intake: body.watch_intake,
        watch_inplace: body.watch_inplace,
    };
    let handle = state.store();
    match state.http_work.run_to_completion(move || {
        super::store_wait::with_bounded_store(handle, |store| {
            store.patch_transfer_settings(body.expected_revision, patch)
        })
    }).await {
        Ok(Some(Ok(settings))) => ok(serde_json::json!(settings)).into_response(),
        Ok(Some(Err(error))) => failure(error),
        Ok(None) => super::store_wait::busy_response(),
        Err(error) => super::library::browse_work_error(error),
    }
}

#[cfg(test)]
mod tests;

fn failure(error: store::StoreError) -> Response {
    tracing::error!(%error, "Transfer settings operation rejected");
    match error {
        store::StoreError::Conflict(message) => {
            err(StatusCode::CONFLICT, "transfer.conflict", &message)
        }
        store::StoreError::Invalid(message) => {
            err(StatusCode::BAD_REQUEST, "transfer.invalid", &message)
        }
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}
