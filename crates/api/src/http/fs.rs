//! Server filesystem directory browser for directory pickers.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
pub async fn browse_fs(
    State(_state): State<ApiState>,
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

    if !target.exists() {
        return err(
            StatusCode::NOT_FOUND,
            "fs.not_found",
            "指定的目录在服务器上不存在",
        );
    }

    if !target.is_dir() {
        return err(
            StatusCode::BAD_REQUEST,
            "fs.not_directory",
            "指定的路径不是一个目录",
        );
    }

    let canonical = target.canonicalize().unwrap_or(target.clone());
    let canonical_str = canonical.display().to_string();

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
            list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            list
        }
        Err(e) => {
            return err(
                StatusCode::FORBIDDEN,
                "fs.access_denied",
                &format!("无法读取目录内容: {e}"),
            );
        }
    };

    ok(serde_json::to_value(FsBrowse {
        path: canonical_str,
        parent,
        entries,
    })
    .unwrap_or_default())
    .into_response()
}
