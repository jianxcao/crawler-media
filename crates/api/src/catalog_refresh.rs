use std::str::FromStr;

use domain::{Media, MediaId};

use crate::management::ApiState;

pub fn run(state: &ApiState, payload: &str) -> Result<(), String> {
    let media_id = payload_media_id(payload)?;
    let store = state.store.lock();
    let stored = store
        .get_media(media_id)
        .map_err(|e| e.to_string())?
        .ok_or("Media not found")?;
    let target = store
        .media_refresh_alias(media_id, "tmdb")
        .map_err(|e| e.to_string())?;
    let Some(refresh_target) = target.or_else(|| {
        stored.tmdb_id.clone().map(|ext_id| domain::MediaAlias {
            source: "tmdb".into(),
            kind: stored.kind,
            external_id: ext_id,
        })
    }) else {
        return Ok(());
    };
    let tmdb_id = refresh_target.external_id;
    let rev = store
        .media_revision(media_id)
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    drop(store);
    tracing::debug!(media = %stored.title, tmdb_id = %tmdb_id, "刷新目录元数据");
    let Some(fresh) = state
        .catalog
        .details(stored.kind, &tmdb_id)
        .map_err(|e| {
            tracing::error!(media = %stored.title, tmdb_id = %tmdb_id, error = %e, "从目录拉取最新元数据失败");
            e.to_string()
        })?
    else {
        tracing::warn!(media = %stored.title, tmdb_id = %tmdb_id, "目录未返回详情");
        return Ok(());
    };
    let updated = apply_details(stored, fresh);
    tracing::info!(media = %updated.title, "已从目录更新媒体元数据");
    state
        .store
        .lock()
        .update_media_identity(rev, &updated)
        .map_err(|e| e.to_string())?;

    let meta_service =
        crate::metadata_service::MetadataService::new(std::sync::Arc::new(state.clone()));
    if let Err(e) = meta_service.refresh_media_metadata(&updated) {
        tracing::warn!(media = %updated.title, error = %e, "刷新采纳元数据失败");
    }

    Ok(())
}

fn apply_details(stored: Media, fresh: Media) -> Media {
    Media {
        id: stored.id,
        kind: stored.kind,
        title: if fresh.title.is_empty() {
            stored.title
        } else {
            fresh.title
        },
        year: fresh.year.or(stored.year),
        original_title: fresh.original_title.or(stored.original_title),
        aliases: if fresh.aliases.is_empty() {
            stored.aliases
        } else {
            fresh.aliases
        },
        tmdb_id: fresh.tmdb_id.or(stored.tmdb_id),
        douban_id: fresh.douban_id.or(stored.douban_id),
        tvdb_id: fresh.tvdb_id.or(stored.tvdb_id),
        bangumi_id: fresh.bangumi_id.or(stored.bangumi_id),
        anilist_id: fresh.anilist_id.or(stored.anilist_id),
    }
}

fn payload_media_id(payload: &str) -> Result<MediaId, String> {
    let value: serde_json::Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    let raw = value
        .get("media_id")
        .and_then(|id| id.as_str())
        .ok_or("catalog_refresh payload missing media_id")?;
    MediaId::from_str(raw).map_err(|e| e.to_string())
}
