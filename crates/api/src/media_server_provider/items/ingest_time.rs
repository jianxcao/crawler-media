//! Observe media creation times after releasing database readers.
use std::{collections::HashMap, time::SystemTime};

/// 锁外：把「可见行的 ingest 时间」按 Media 取最大值。
pub(in crate::media_server_provider) fn latest_ingest_by_media(
    browse: &dyn crate::file_stat_read::FileStatRead,
    paths: &[(domain::MediaId, String)],
) -> HashMap<domain::MediaId, SystemTime> {
    let mut latest = HashMap::new();
    for (media_id, path) in paths {
        let Some(created) = file_date_created(browse, std::path::Path::new(path)) else {
            continue;
        };
        latest
            .entry(*media_id)
            .and_modify(|current: &mut SystemTime| *current = (*current).max(created))
            .or_insert(created);
    }
    latest
}

pub(in crate::media_server_provider) fn file_date_created(
    browse: &dyn crate::file_stat_read::FileStatRead,
    path: &std::path::Path,
) -> Option<SystemTime> {
    let metadata = match browse.metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "Jellyfin DateCreated unavailable for media file");
            return None;
        }
    };
    let created = metadata.created().or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "Jellyfin DateCreated uses modified time because creation time is unavailable");
        metadata.modified()
    });
    match created {
        Ok(created) => Some(created),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "Jellyfin DateCreated unavailable for media file");
            None
        }
    }
}

pub(in crate::media_server_provider) fn with_date_created(
    mut metadata: media_server::provider::MediaItemMetadata,
    date_created: Option<SystemTime>,
) -> media_server::provider::MediaItemMetadata {
    metadata.date_created = date_created.and_then(crate::compact_time::format_rfc3339_utc);
    metadata
}
