//! Server filesystem directory browser for directory pickers.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::http::{err, ok};
use crate::management::ApiState;

#[derive(Deserialize)]
pub struct BrowseQuery {
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct FsBrowse {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
}

/// GET /api/v1/fs/browse?path=...
/// Browse directories on the server.
///
/// 目录枚举（`exists`/`is_dir`/`canonicalize`/`read_dir`/逐项 `is_dir`）可能落在
/// 网络存储或超大目录上，因此**整体**放进 blocking 线程：async worker 上做这些
/// 同步调用会把运行时一起拖住。
///
/// Filesystem readers share a bounded queue, separate from database/HTTP work.
pub async fn browse_fs(
    State(state): State<ApiState>,
    Query(query): Query<BrowseQuery>,
) -> Response {
    let target = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(unix)]
            {
                PathBuf::from("/")
            }
            #[cfg(not(unix))]
            {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }
        });

    match read_work(&state, move || read_directory(&target)).await {
        Ok(Ok(browse)) => ok(serde_json::to_value(browse).unwrap_or_default()).into_response(),
        Ok(Err(response)) => response,
        Err(
            crate::blocking_budget::BlockingError::Busy
            | crate::blocking_budget::BlockingError::Timeout,
        ) => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "fs.busy",
            "文件系统浏览并发过载",
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "fs.worker",
            &error.to_string(),
        ),
    }
}

/// Read-only filesystem work queues before starting a blocking worker. A burst
/// of artwork/scrape IO must not turn unrelated maintenance reads into immediate
/// 503s. The ten second deadline includes both queueing and actual observation;
/// a timed out worker keeps its permit until its filesystem calls finish.
pub(crate) async fn read_work<T: Send + 'static>(
    state: &ApiState,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, crate::blocking_budget::BlockingError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let result = async {
        state
            .fs_work
            .reserve(super::store_wait::STORE_READ_WAIT)
            .await?
            .run(
                deadline.saturating_duration_since(std::time::Instant::now()),
                work,
            )
            .await
    }
    .await;
    if let Err(error) = &result {
        match error {
            crate::blocking_budget::BlockingError::Worker(_) => {
                tracing::error!(%error, "filesystem read worker failed");
            }
            _ => tracing::warn!(%error, "filesystem read did not finish within its budget"),
        }
    }
    result
}

/// 同步的目录枚举。状态码与文案保持既有行为：不存在 404、不是目录 400、
/// 读不了 403。隐藏目录（`.` 开头）不列出，按名称大小写不敏感排序。
fn read_directory(target: &Path) -> Result<FsBrowse, Response> {
    if !target.exists() {
        return Err(err(
            StatusCode::NOT_FOUND,
            "fs.not_found",
            "指定的目录在服务器上不存在",
        ));
    }
    if !target.is_dir() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "fs.not_directory",
            "指定的路径不是一个目录",
        ));
    }

    let canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let parent = canonical.parent().map(|p| p.display().to_string());

    let entries = match fs::read_dir(&canonical) {
        Ok(read_dir) => {
            let mut list = Vec::new();
            for entry in read_dir.flatten() {
                let path = entry.path();
                // Only list directories, skip hidden ones (.git, .DS_Store, etc.)
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    list.push(FsEntry {
                        name: name.to_string(),
                        path: path.display().to_string(),
                    });
                }
            }
            list.sort_by_key(|a| a.name.to_lowercase());
            list
        }
        Err(e) => {
            return Err(err(
                StatusCode::FORBIDDEN,
                "fs.access_denied",
                &format!("无法读取目录内容: {e}"),
            ));
        }
    };

    Ok(FsBrowse {
        path: canonical.display().to_string(),
        parent,
        entries,
    })
}
