use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    http::{err, ok},
    management::ApiState,
    probe_settings::{ProbeSettings, RemoteReadsSettings},
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

pub(crate) async fn get_remote_reads(State(state): State<ApiState>) -> Response {
    super::store_wait::store_read(
        &state,
        &[store::Db::App],
        |store| match RemoteReadsSettings::load(store) {
            Ok(settings) => ok(serde_json::json!(settings)).into_response(),
            Err(error) => store_error(error),
        },
    )
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn put_remote_reads(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(settings) = serde_json::from_value::<RemoteReadsSettings>(body) else {
        return err(
            StatusCode::BAD_REQUEST,
            "probe.invalid",
            "暂停状态需要布尔值",
        );
    };
    let probe = state.probe.clone();
    let data = serde_json::json!(settings);
    let raw = data.to_string();
    super::store_wait::store_write(&state, move |store| {
        let saved = store.writers().with_app(|app| {
            app.put_setting(crate::settings_keys::PROBE_REMOTE_READS, &raw)?;
            probe.apply_remote_reads_pause(settings.paused);
            Ok(())
        });
        match saved {
            Ok(()) => {
                tracing::info!(paused = settings.paused, "STRM 后台读取设置已保存并生效");
                ok(data).into_response()
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

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelProbeRequest {
    library_id: Option<String>,
}

fn status_json(
    paused: bool,
    remote_reads_paused: bool,
    summary: store::ProbeQueueSummary,
) -> Value {
    serde_json::json!({
        "paused": paused,
        "remote_reads_paused": remote_reads_paused,
        "queued_metadata": summary.queued_metadata,
        "running_metadata": summary.running_metadata,
        "queued_fingerprint": summary.queued_fingerprint,
        "running_fingerprint": summary.running_fingerprint,
        "pending_obligations": summary.pending_obligations,
        "total_pending": summary.total_pending,
        "libraries": summary.libraries,
    })
}

async fn build_status_response(state: &ApiState) -> Response {
    let paused = state.probe.background_is_paused();
    let remote_reads_paused = state.probe.remote_reads_are_paused();
    super::store_wait::store_read(state, &[store::Db::Library], move |store| {
        match store.probe_queue_summary() {
            Ok(summary) => ok(status_json(paused, remote_reads_paused, summary)).into_response(),
            Err(error) => store_error(error),
        }
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn status(State(state): State<ApiState>) -> Response {
    build_status_response(&state).await
}

pub(crate) async fn pause(State(state): State<ApiState>) -> Response {
    state.probe.pause_background();
    tracing::info!("后台批量媒体探测队列已暂停");
    build_status_response(&state).await
}

pub(crate) async fn resume(State(state): State<ApiState>) -> Response {
    state.probe.set_background_paused_flag(false);
    let worker_state = state.clone();
    if let Err(error) = state
        .http_work
        .run_to_completion(move || {
            if let Err(err) = crate::scan_probe_dispatch::dispatch_obligations_sync(
                &worker_state.store,
                &worker_state.probe,
            ) {
                tracing::warn!(%err, "恢复后台探测时派发待办义务失败，等待后台重试");
            }
            worker_state.probe.resume_background();
        })
        .await
    {
        tracing::warn!(%error, "恢复后台探测即时补充窗口繁忙，交由后台调度器跟进");
        state.probe.wake_dispatch();
    }
    tracing::info!("后台批量媒体探测队列已手动恢复执行");
    build_status_response(&state).await
}

pub(crate) async fn cancel(State(state): State<ApiState>, body: Bytes) -> Response {
    let trimmed = body
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'));
    let parsed = if trimmed {
        CancelProbeRequest::default()
    } else {
        match serde_json::from_slice::<CancelProbeRequest>(&body) {
            Ok(req) => req,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "probe.invalid",
                    "取消探测请求参数格式无效",
                );
            }
        }
    };
    let library_id = parsed
        .library_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let global_cancel = library_id.is_none();
    let result = super::store_wait::store_write(&state, move |store| {
        store
            .library_write(|db| db.cancel_queued_probe_work(library_id.as_deref(), "manual_cancel"))
    })
    .await;
    match result {
        Ok(Ok(summary)) => {
            state
                .probe
                .purge_cancelled_seen(&summary.cancelled_file_ids, global_cancel);
            tracing::info!(
                cancelled_units = summary.cancelled_units,
                cleared_obligations = summary.cleared_obligations,
                global_cancel,
                "已清空排队中的后台探测任务"
            );
            ok(serde_json::json!({
                "cancelled_units": summary.cancelled_units,
                "cleared_obligations": summary.cleared_obligations,
            }))
            .into_response()
        }
        Ok(Err(error)) => store_error(error),
        Err(response) => response,
    }
}
