//! Manual file artwork writes own one bounded worker through publication.
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::artwork_key;
use crate::http::{err, ok, store_wait};
use crate::management::ApiState;

const MAX_ARTWORK_BYTES: usize = 12 * 1024 * 1024;

struct Target {
    coverage: domain::FileCoverage,
    media: domain::Media,
    path: std::path::PathBuf,
    enabled_roots: Vec<std::path::PathBuf>,
}

fn capture(
    state: &ApiState,
    library: &str,
    media: domain::MediaId,
    backdrop: bool,
) -> Result<Target, Response> {
    store_wait::with_foreground_store(state, |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        let library_id: domain::LibraryId = library.parse().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "library.invalid",
                "Library id 无效",
            )
        })?;
        let library_id_str = library_id.to_string();
        let enabled_roots = store
            .list_library_roots()
            .map_err(storage_error)?
            .into_iter()
            .filter(|root| root.enabled && root.library_id == library_id_str)
            .map(|root| root.path)
            .collect();
        let rows = store
            .file_coverage_in_library(library_id, Some(media), store::FileReadScope::Published)
            .map_err(storage_error)?;
        let coverage = super::super::library::preferred_coverage(&rows)
            .cloned()
            .ok_or_else(|| {
                err(
                    StatusCode::NOT_FOUND,
                    "library.item_missing",
                    "条目没有可用文件",
                )
            })?;
        let media = store
            .get_media(media)
            .map_err(storage_error)?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
        let path = coverage
            .file
            .path
            .parent()
            .ok_or_else(|| err(StatusCode::CONFLICT, "artwork.path", "无法定位封面目录"))?
            .join(if backdrop { "fanart.jpg" } else { "poster.jpg" });
        Ok(Target {
            coverage,
            media,
            path,
            enabled_roots,
        })
    })?
}

fn validate(store: &crate::Store, state: &ApiState, target: &Target) -> Result<(), Response> {
    state.require_writable()?;
    state.require_write_admitted()?;
    let current = store
        .file_coverage_by_id(target.coverage.file.file_id)
        .map_err(storage_error)?;
    if !current.as_ref().is_some_and(|current| {
        super::super::file_delete::same_recorded_source(&target.coverage, current)
    }) {
        return Err(err(
            StatusCode::CONFLICT,
            "artwork.stale",
            "文件版本或归属已变化",
        ));
    }
    if !store
        .library_accepts_work(&target.coverage.file.library_id.to_string())
        .map_err(storage_error)?
    {
        return Err(err(
            StatusCode::CONFLICT,
            "library.deleting",
            "Library 正在删除",
        ));
    }
    Ok(())
}

fn update_lock(state: &ApiState, target: &Target, source: Option<&str>) -> Result<(), Response> {
    store_wait::with_foreground_store(state, |store| {
        validate(store, state, target)?;
        match source {
            Some(source) => store.put_setting(
                &artwork_key(target.media.id),
                &json!({"poster_locked":true,"source":source}).to_string(),
            ),
            None => store
                .delete_setting(&artwork_key(target.media.id))
                .map(|_| ()),
        }
        .map_err(storage_error)
    })?
}

fn storage_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "file artwork storage operation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "artwork.storage",
        &error.to_string(),
    )
}

fn publish_bytes(
    state: &ApiState,
    target: &Target,
    bytes: &[u8],
    source: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Response {
    let published = lease.publish(|| {
        store_wait::with_foreground_store(state, |store| validate(store, state, target))
            .and_then(std::convert::identity)
            .map_err(|response| format!("artwork rejected ({})", response.status()))?;
        std::fs::write(&target.path, bytes).map_err(|error| error.to_string())
    });
    if let Err(error) = published {
        tracing::error!(%error, path=%target.path.display(), "manual artwork publication failed");
        let code = if error == "Library work cancelled" {
            "library.deleting"
        } else {
            "artwork.write"
        };
        return err(StatusCode::CONFLICT, code, &error);
    }
    crate::poster_fetch::record_discovered_artwork(state, &target.coverage.file.path);
    if let Err(response) = update_lock(state, target, Some(source)) {
        return response;
    }
    tracing::info!(media_id=%target.media.id, path=%target.path.display(), bytes=bytes.len(), "published manual artwork");
    ok(json!({"locked":true,"automatic":false,"bytes":bytes.len()})).into_response()
}

fn write_artwork(
    state: &ApiState,
    library: String,
    media: domain::MediaId,
    body: Value,
    upload: bool,
) -> Response {
    use base64::Engine;
    let lease = match state.library_work.acquire_bounded(&state.store(), &library) {
        Ok(lease) => lease,
        Err(
            crate::library_lifecycle::BoundedLeaseError::StoreBusy
            | crate::library_lifecycle::BoundedLeaseError::RegistryBusy,
        ) => return store_wait::busy_response(),
        Err(error) => return err(StatusCode::CONFLICT, "library.deleting", &error.to_string()),
    };
    let backdrop = body.get("kind").and_then(Value::as_str) == Some("backdrop");
    let target = match capture(state, &library, media, backdrop) {
        Ok(target) => target,
        Err(response) => return response,
    };
    if upload {
        let encoded = body["data_url"]
            .as_str()
            .and_then(|url| url.split_once(','))
            .map(|(_, encoded)| encoded);
        let bytes = encoded.and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        });
        let Some(bytes) =
            bytes.filter(|bytes| !bytes.is_empty() && bytes.len() <= 12 * 1024 * 1024)
        else {
            return err(
                StatusCode::BAD_REQUEST,
                "artwork.upload",
                "图片编码无效、为空或超过 12MB",
            );
        };
        return publish_bytes(state, &target, &bytes, "upload", &lease);
    }
    let Some(source) = body["file_path"].as_str() else {
        if let Err(response) = update_lock(state, &target, None) {
            return response;
        }
        if backdrop {
            crate::poster_fetch::attach_backdrop(state, &target.media, &target.coverage.file.path);
        } else {
            crate::poster_fetch::attach_poster(state, &target.media, &target.coverage.file.path);
        }
        return ok(json!({"locked":false,"automatic":true})).into_response();
    };
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        match fetch_http_candidate(state, source) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        }
    } else {
        match read_local_candidate(source, &target.enabled_roots) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        }
    };
    publish_bytes(state, &target, &bytes, source, &lease)
}

fn has_valid_image_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn invalid_candidate(source: &str, reason: &str) -> Response {
    tracing::warn!(source, reason, "rejected manual artwork candidate");
    err(StatusCode::BAD_REQUEST, "artwork.invalid", "候选图片无效")
}

fn read_local_candidate(
    source: &str,
    enabled_roots: &[std::path::PathBuf],
) -> Result<Vec<u8>, Response> {
    use std::io::Read;
    let canon_source = std::fs::canonicalize(source)
        .map_err(|_| invalid_candidate(source, "canonicalize failed"))?;
    let inside_enabled_root = enabled_roots.iter().any(|root| {
        std::fs::canonicalize(root).is_ok_and(|canon_root| {
            canon_source != canon_root && canon_source.starts_with(&canon_root)
        })
    });
    if !inside_enabled_root {
        return Err(invalid_candidate(source, "outside enabled library roots"));
    }
    if !std::fs::metadata(&canon_source).is_ok_and(|meta| meta.is_file()) {
        return Err(invalid_candidate(source, "not a regular file"));
    }
    let file =
        std::fs::File::open(&canon_source).map_err(|_| invalid_candidate(source, "open failed"))?;
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Err(invalid_candidate(
            source,
            "opened handle is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTWORK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_candidate(source, "read failed"))?;
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES || !has_valid_image_magic(&bytes) {
        return Err(invalid_candidate(
            source,
            "empty, oversized, or invalid image header",
        ));
    }
    Ok(bytes)
}

fn fetch_http_candidate(state: &ApiState, source: &str) -> Result<Vec<u8>, Response> {
    if !crate::http::image_proxy::allowed_url(source) {
        return Err(invalid_candidate(source, "disallowed artwork URL"));
    }
    let bytes = state.poster_fetch.get(source).map_err(|error| {
        if error.contains("disallowed") || error.contains("exceeds") {
            invalid_candidate(source, &error)
        } else {
            tracing::warn!(source, %error, "manual artwork candidate could not be read");
            err(StatusCode::BAD_GATEWAY, "artwork.fetch", "读取候选图片失败")
        }
    })?;
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES || !has_valid_image_magic(&bytes) {
        return Err(invalid_candidate(
            source,
            "empty, oversized, or invalid image header",
        ));
    }
    Ok(bytes)
}

async fn mutation(
    state: ApiState,
    library: String,
    item: String,
    body: Value,
    upload: bool,
) -> Response {
    let media = match item.parse() {
        Ok(media) => media,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let worker = state.clone();
    match state
        .fs_work
        .run_to_completion(move || write_artwork(&worker, library, media, body, upload))
        .await
    {
        Ok(response) => response,
        Err(error) => super::super::library::browse_work_error(error),
    }
}

pub(crate) async fn select_artwork(
    State(state): State<ApiState>,
    Path((id, item)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    mutation(state, id, item, body, false).await
}

pub(crate) async fn upload_artwork(
    State(state): State<ApiState>,
    Path((id, item)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    mutation(state, id, item, body, true).await
}
