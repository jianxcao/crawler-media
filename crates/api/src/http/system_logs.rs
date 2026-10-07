use std::collections::HashMap;
use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::http::ok;
use crate::management::ApiState;

/// Query parameters for GET /api/v1/system/logs
pub(crate) async fn list_logs(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let level = query.get("level").map(String::as_str);
    let target = query
        .get("target")
        .or_else(|| query.get("module"))
        .map(String::as_str);
    let keyword = query
        .get("q")
        .or_else(|| query.get("keyword"))
        .map(String::as_str);
    let limit = query.get("limit").and_then(|l| l.parse::<usize>().ok());

    let entries = state.log_buffer.query(level, target, keyword, limit);
    ok(json!({
        "total": entries.len(),
        "entries": entries,
    }))
    .into_response()
}

/// Clear in-memory log buffer
pub(crate) async fn clear_logs(State(state): State<ApiState>) -> Response {
    state.log_buffer.clear();
    ok(json!({ "cleared": true })).into_response()
}

/// Export logs as plain text
pub(crate) async fn export_logs(State(state): State<ApiState>) -> Response {
    let data_dir = {
        let store = state.store.lock();
        store.data_dir().to_path_buf()
    };
    let logs_dir = data_dir.join("logs");
    // 支持直接匹配 crawler-media.log 或 tracing-appender 生成的 daily 文件 crawler-media.log.YYYY-MM-DD
    let mut file_content = None;
    if logs_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&logs_dir) {
            let mut log_files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("crawler-media.log"))
                        .unwrap_or(false)
                })
                .collect();
            log_files.sort();
            if let Some(latest) = log_files.last() {
                file_content = std::fs::read_to_string(latest).ok();
            }
        }
    }
    let content = file_content.unwrap_or_else(|| export_from_buffer(&state));

    let filename = format!("crawler-media-logs-{}.txt", chrono_like_timestamp());

    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                &format!(r#"attachment; filename="{filename}""#),
            ),
        ],
        content,
    )
        .into_response()
}

fn export_from_buffer(state: &ApiState) -> String {
    let entries = state.log_buffer.query(None, None, None, None);
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "[{}] [{}] [{}] {}\n",
            format_timestamp(entry.timestamp),
            entry.level,
            entry.target,
            entry.message
        ));
    }
    out
}

fn format_timestamp(ms: i64) -> String {
    // Simple ISO-ish representation without external chrono dependency
    let secs = ms / 1000;
    let millis = ms % 1000;
    format!("{secs}.{millis:03}")
}

fn chrono_like_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// SSE stream for real-time logs
pub(crate) async fn stream_logs(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_buffer.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(entry) => {
            let data = serde_json::to_string(&entry).unwrap_or_default();
            Some(Ok(Event::default().event("log").data(data)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
