use std::collections::HashMap;
use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use crate::http::ok;
use crate::management::ApiState;

/// Query parameters for GET /api/v1/system/logs
pub(crate) async fn list_logs(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(key) = ["module", "keyword"]
        .iter()
        .find(|k| query.contains_key(**k))
    {
        return crate::http::err(
            axum::http::StatusCode::BAD_REQUEST,
            "logs.invalid_query",
            &format!("unknown query key {key}; use target/q"),
        );
    }
    let level = query.get("level").map(String::as_str);
    let target = query.get("target").map(String::as_str);
    let keyword = query.get("q").map(String::as_str);
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

/// 单次读块。完整文件仍全部下载，驻留内存停在这一块加上 [`EXPORT_PREFETCH`] 个预读。
const EXPORT_CHUNK: usize = 256 * 1024;
/// 客户端尚未取走时，阻塞读线程最多再持有的块数。
const EXPORT_PREFETCH: usize = 2;

/// Export logs as plain text.
///
/// `data_dir` 在 Store 打开后不变，不进读快照，因此不等 writer。
/// 目录选择、打开和按块读取都在 blocking 线程。命中文件只读打开瞬间的长度，
/// 之后追加的日志留给下一次导出，响应体与 Content-Length 一致。
pub(crate) async fn export_logs(State(state): State<ApiState>) -> Response {
    let logs_dir = state.store.data_dir().join("logs");
    let filename = format!("crawler-media-logs-{}.txt", chrono_like_timestamp());
    let selected = tokio::task::spawn_blocking(move || observe_latest_log(&logs_dir))
        .await
        .unwrap_or(ObservedLog::Missing);
    let ObservedLog::Opened { file, length } = selected else {
        return buffer_export(&state, &filename);
    };
    file_export(file, length, &filename)
}

enum ObservedLog {
    Missing,
    Opened {
        file: std::fs::File,
        length: Option<u64>,
    },
}

fn observe_latest_log(logs_dir: &std::path::Path) -> ObservedLog {
    let Some(path) = latest_log_file(logs_dir) else {
        return ObservedLog::Missing;
    };
    match std::fs::File::open(&path) {
        Ok(file) => {
            let length = file.metadata().ok().map(|meta| meta.len());
            ObservedLog::Opened { file, length }
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "打开日志文件失败，回退内存缓冲");
            ObservedLog::Missing
        }
    }
}

fn file_export(file: std::fs::File, length: Option<u64>, filename: &str) -> Response {
    let mut response = Response::new(Body::from_stream(bounded_snapshot_stream(file, length)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Some(length) = length {
        headers.insert(
            header::CONTENT_LENGTH,
            axum::http::HeaderValue::from(length),
        );
    }
    response
}

fn bounded_snapshot_stream(
    file: std::fs::File,
    length: Option<u64>,
) -> ReceiverStream<Result<axum::body::Bytes, std::io::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(EXPORT_PREFETCH);
    tokio::task::spawn_blocking(move || read_snapshot(file, length, tx));
    ReceiverStream::new(rx)
}

fn read_snapshot(
    mut file: std::fs::File,
    length: Option<u64>,
    tx: tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
) {
    use std::io::Read;
    let mut remaining = length.unwrap_or(u64::MAX);
    let mut buf = vec![0u8; EXPORT_CHUNK];
    let mut filled = 0usize;
    while remaining > 0 {
        let room = EXPORT_CHUNK - filled;
        let want = room.min(usize::try_from(remaining).unwrap_or(room));
        match file.read(&mut buf[filled..filled + want]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                remaining -= n as u64;
                if filled == EXPORT_CHUNK || remaining == 0 {
                    if tx
                        .blocking_send(Ok(axum::body::Bytes::copy_from_slice(&buf[..filled])))
                        .is_err()
                    {
                        return;
                    }
                    filled = 0;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = tx.blocking_send(Err(error));
                return;
            }
        }
    }
    if filled > 0 {
        let _ = tx.blocking_send(Ok(axum::body::Bytes::copy_from_slice(&buf[..filled])));
    }
}

/// 既有语义：`logs/` 下 `crawler-media.log*` 里字典序最大的文件。
fn latest_log_file(logs_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if !logs_dir.is_dir() {
        return None;
    }
    let mut files: Vec<_> = std::fs::read_dir(logs_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("crawler-media.log"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files.pop()
}

fn content_disposition(filename: &str) -> String {
    format!(r#"attachment; filename="{filename}""#)
}

/// 内存缓冲回退（≤3000 条，行为保持原样）。
fn buffer_export(state: &ApiState, filename: &str) -> Response {
    let content = export_from_buffer(state);
    let mut response = content.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
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
