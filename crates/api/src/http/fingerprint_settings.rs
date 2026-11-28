//! Standalone HTTP controls for the audio fingerprint intro/outro queue.
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    http::{err, ok},
    management::ApiState,
    probe_settings::FingerprintProbeSettings,
};

pub(crate) async fn get(State(state): State<ApiState>) -> Response {
    super::store_wait::store_read(&state, &[store::Db::App], |store| {
        match FingerprintProbeSettings::load(store) {
            Ok(settings) => ok(json!(settings)).into_response(),
            Err(error) => store_error(error),
        }
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn put(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let Ok(settings) = serde_json::from_value::<FingerprintProbeSettings>(body) else {
        return err(
            StatusCode::BAD_REQUEST,
            "probe.invalid",
            "片头片尾声纹设置需要完整的并发数与暂停布尔值",
        );
    };
    if let Err(message) = settings.validate() {
        return err(StatusCode::BAD_REQUEST, "probe.invalid", &message);
    }
    save_and_apply(&state, settings).await.unwrap_or_else(|r| r)
}

async fn persist_and_publish_settings<F>(
    state: &ApiState,
    compute_next: F,
) -> Result<FingerprintProbeSettings, Response>
where
    F: FnOnce(&store::App<'_>) -> Result<FingerprintProbeSettings, store::StoreError>
        + Send
        + 'static,
{
    let probe = state.probe.clone();
    let next = super::store_wait::store_write(state, move |store| {
        store
            .writers()
            .with_app(|app| {
                let next = compute_next(app)?;
                let raw = serde_json::to_string(&next)?;
                app.put_setting(crate::settings_keys::PROBE_FINGERPRINT, &raw)?;
                // Keep publication ordered with concurrent saves. Pure in-memory admission
                // update; performs no DB/file IO and never interrupts active reads.
                probe.publish_fingerprint_settings(next);
                Ok(next)
            })
            .map_err(store_error)
    })
    .await??;
    if let Some(hook) = state.fingerprint_settings_after_write_barrier.clone() {
        let _ = tokio::task::spawn_blocking(move || hook(next.workers, next.paused)).await;
    }
    let probe = state.probe.clone();
    if let Err(error) = tokio::task::spawn_blocking(move || {
        probe.reconcile_fingerprint_dispatch_after_settings();
    })
    .await
    {
        tracing::warn!(%error, "声纹设置生效后重协调派发窗口失败，等待后台轮询兜底");
        state.probe.wake_dispatch();
    }
    Ok(next)
}

async fn save_and_apply(
    state: &ApiState,
    settings: FingerprintProbeSettings,
) -> Result<Response, Response> {
    let saved = persist_and_publish_settings(state, move |_| Ok(settings)).await?;
    tracing::info!(?saved, "片头片尾声纹队列设置已保存并生效");
    Ok(ok(json!(saved)).into_response())
}

async fn update_paused(state: &ApiState, paused: bool) -> Result<(), Response> {
    let _ = persist_and_publish_settings(state, move |app| {
        let current = match app.get_setting(crate::settings_keys::PROBE_FINGERPRINT)? {
            Some(raw) => {
                let parsed = serde_json::from_str::<FingerprintProbeSettings>(&raw)?;
                parsed.validate().map_err(store::StoreError::Invalid)?;
                parsed
            }
            None => FingerprintProbeSettings::default(),
        };
        Ok(FingerprintProbeSettings {
            workers: current.workers,
            paused,
        })
    })
    .await?;
    Ok(())
}

async fn build_status_response(state: &ApiState) -> Response {
    let paused = state.probe.fingerprint_is_paused();
    let background_paused = state.probe.background_is_paused();
    let remote_reads_paused = state.probe.remote_reads_are_paused();
    let workers = state.probe.fingerprint_worker_limit();
    let active_workers = state.probe.active_fingerprint_workers();
    super::store_wait::store_read(state, &[store::Db::Library], move |store| {
        let summary = match store.probe_queue_summary() {
            Ok(summary) => summary,
            Err(error) => return store_error(error),
        };
        let queued_analysis = match store.queued_analysis_unit_count() {
            Ok(count) => count,
            Err(error) => return store_error(error),
        };
        ok(json!({
            "paused": paused,
            "background_paused": background_paused,
            "remote_reads_paused": remote_reads_paused,
            "workers": workers,
            "active_workers": active_workers,
            "queued_fingerprint": summary.queued_fingerprint,
            "running_fingerprint": summary.running_fingerprint,
            "queued_analysis": queued_analysis,
        }))
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

pub(crate) async fn status(State(state): State<ApiState>) -> Response {
    build_status_response(&state).await
}

pub(crate) async fn pause(State(state): State<ApiState>) -> Response {
    if let Err(response) = update_paused(&state, true).await {
        return response;
    }
    tracing::info!("片头片尾声纹独立队列已暂停（不影响媒体信息探测）");
    build_status_response(&state).await
}

pub(crate) async fn resume(State(state): State<ApiState>) -> Response {
    if let Err(response) = update_paused(&state, false).await {
        return response;
    }
    tracing::info!("片头片尾声纹独立队列已恢复执行");
    build_status_response(&state).await
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelFingerprintRequest {
    library_id: Option<String>,
    media_id: Option<String>,
    season: Option<u32>,
    file_id: Option<String>,
    include_running: Option<bool>,
}

pub(crate) async fn cancel(State(state): State<ApiState>, body: Bytes) -> Response {
    let trimmed = body
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'));
    let parsed = if trimmed {
        CancelFingerprintRequest::default()
    } else {
        match serde_json::from_slice::<CancelFingerprintRequest>(&body) {
            Ok(req) => req,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "probe.invalid",
                    "取消片头片尾任务参数格式无效",
                );
            }
        }
    };
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let scope = store::FingerprintCancelScope {
        library_id: clean(parsed.library_id),
        media_id: clean(parsed.media_id),
        season: parsed.season,
        file_id: clean(parsed.file_id),
        include_running: parsed.include_running.unwrap_or(true),
    };
    let global_cancel = scope.library_id.is_none()
        && scope.media_id.is_none()
        && scope.season.is_none()
        && scope.file_id.is_none();
    let worker = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || {
            let outcome = super::store_wait::with_foreground_store(&worker, |store| {
                store.library_write(|db| db.cancel_fingerprint_probe_work(&scope, "manual_cancel"))
            });
            let mut aborted_running = 0;
            if let Ok(Ok(summary)) = &outcome {
                aborted_running = worker
                    .probe
                    .abort_matching_active_fingerprints(&scope, &summary.cancelled_file_ids);
                worker
                    .probe
                    .purge_cancelled_fingerprint(summary, global_cancel);
            }
            (outcome, aborted_running)
        })
        .await;
    match result {
        Ok((Ok(Ok(summary)), aborted_running)) => {
            tracing::info!(
                cancelled_units = summary.cancelled_units,
                aborted_running,
                global_cancel,
                "已取消片头片尾声纹任务"
            );
            ok(json!({
                "cancelled_units": summary.cancelled_units,
                "aborted_running": aborted_running,
            }))
            .into_response()
        }
        Ok((Ok(Err(error)), _)) => store_error(error),
        Ok((Err(response), _)) => response,
        Err(crate::blocking_budget::BlockingError::Busy) => super::store_wait::busy_response(),
        Err(error) => super::store_wait::StoreCallError::Worker(error.to_string()).response(),
    }
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "片头片尾声纹队列设置存取失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "片头片尾声纹队列设置存取失败",
    )
}
