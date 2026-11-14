use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::{dto, provider::MediaItemSnapshot};

pub(in crate::routes) async fn media_source(
    snapshot: &MediaItemSnapshot,
    item_id: &str,
) -> Result<Value, StatusCode> {
    let path = snapshot.file.path.clone();
    let is_strm = path
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".strm");
    let source_path = if is_strm {
        tokio::task::spawn_blocking(move || library::read_strm_url(&path))
            .await
            .map_err(|error| {
                tracing::error!(%error, "Jellyfin STRM source task failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .ok_or_else(|| {
                tracing::warn!(item_id, "Jellyfin STRM file has no usable URL");
                StatusCode::NOT_FOUND
            })?
    } else {
        path.to_string_lossy().into_owned()
    };
    let protocol = if source_path.starts_with("http://") || source_path.starts_with("https://") {
        "Http"
    } else {
        "File"
    };
    let container = source_path
        .split(['?', '#'])
        .next()
        .and_then(|path| std::path::Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let source_id = domain::PhysicalSourceId {
        file_id: snapshot.file.file_id,
        revision: snapshot.file.version.revision,
    }
    .to_wire_string();
    let mut source = json!({
        "Id": source_id,
        "Path": source_path,
        "Protocol": protocol,
        "Container": container,
        "RunTimeTicks": snapshot.metadata.runtime_ticks,
        "MediaStreams": dto::media_streams_with_item(&snapshot.metadata.tracks, Some(item_id), Some(&source_id)),
        "IsRemote": is_strm,
        "SupportsDirectPlay": true,
        "SupportsDirectStream": true,
        "SupportsTranscoding": false,
    });
    if let Some(name) = snapshot.metadata.episode_name.as_ref() {
        source["Name"] = json!(name);
    }
    Ok(source)
}
