use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::{
    http::{err, ok},
    management::ApiState,
    probe_settings::ProbeSettings,
};

pub(crate) async fn get(State(state): State<ApiState>) -> Response {
    super::store_wait::store_read(
        &state,
        &[store::Db::App],
        |store| match ProbeSettings::load(store) {
            Ok(settings) => ok(serde_json::json!(settings)).into_response(),
            Err(error) => store_error(error),
        },
    )
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn put(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let settings = match serde_json::from_value::<ProbeSettings>(body) {
        Ok(settings) => settings,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "probe.invalid",
                "探测设置需要完整的整数参数",
            );
        }
    };
    if let Err(message) = settings.validate() {
        return err(StatusCode::BAD_REQUEST, "probe.invalid", &message);
    }
    let probe = state.probe.clone();
    super::store_wait::store_write(&state, move |store| {
        let raw = match serde_json::to_string(&settings) {
            Ok(raw) => raw,
            Err(error) => return store_error(error.into()),
        };
        let saved = store.writers().with_app(|app| {
            app.put_setting(crate::settings_keys::PROBE_SETTINGS, &raw)?;
            // Keep publication ordered with concurrent saves. This only updates
            // admission in memory; it performs no IO and interrupts no work.
            probe.apply_probe_settings(settings);
            Ok(())
        });
        match saved {
            Ok(()) => {
                tracing::info!(?settings, "媒体探测设置已保存并生效");
                ok(serde_json::json!(settings)).into_response()
            }
            Err(error) => store_error(error),
        }
    })
    .await
    .unwrap_or_else(|response| response)
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "媒体探测设置存取失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "探测设置存取失败",
    )
}
