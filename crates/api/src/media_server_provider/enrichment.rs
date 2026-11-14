//! Catalog（TMDB 缓存）元数据合并：单条目路径与条目选择共用一份载荷。
//!
//! 选择路径要先用合并后的 metadata 过滤（`PersonIds` 等），再只为选中的页重建
//! 重元数据，所以这里把「取载荷」与「应用载荷」拆开：同一份载荷可以在轻候选与
//! 页面重元数据上各应用一次，结果一致。

use super::*;
use crate::scrape_store::ScrapeStoreExt;

/// 一次 catalog 合并的载荷（幂等：对同一 metadata 再合并一次结果相同）。
pub(super) struct CatalogMerge {
    metadata: media::ItemMeta,
    episode: Option<media::EpisodeMeta>,
}

impl CatalogMerge {
    pub(super) fn apply(
        &self,
        metadata: media_server::provider::MediaItemMetadata,
        media: &domain::Media,
    ) -> media_server::provider::MediaItemMetadata {
        merge_catalog_metadata(metadata, media, &self.metadata, self.episode.as_ref())
    }
}

/// 锁外同步取载荷：只读取现有 catalog 缓存，调用方不得持有 Store 锁。
///
/// 单条目 enrich 把读取失败当成「没有 catalog」：那只影响可选字段。选择路径在
/// person 过滤需要这份事实时必须用 [`catalog_merge_required`]，失败要向上抛。
pub(super) fn catalog_merge(
    state: &ApiState,
    snapshot: &MediaItemSnapshot,
) -> Option<CatalogMerge> {
    match catalog_merge_required(state, snapshot) {
        Ok(merge) => merge,
        Err(error) => {
            tracing::warn!(%error, "failed to load optional catalog metadata for Jellyfin item");
            None
        }
    }
}

/// 选择路径用：catalog 读取失败是错误，不是「这个人没出现」。
pub(super) fn catalog_merge_required(
    state: &ApiState,
    snapshot: &MediaItemSnapshot,
) -> Result<Option<CatalogMerge>, String> {
    let Some(tmdb_id) = snapshot.media.tmdb_id.clone() else {
        return Ok(None);
    };
    let preferences = state
        .store
        .get_scrape_config()
        .map_err(|error| error.to_string())?
        .effective;
    let metadata = match state.catalog.cached_metadata_with_preferences(
        snapshot.media.kind,
        &tmdb_id,
        &preferences.language_priority,
        &preferences.cert_country_priority,
    ) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => {
            tracing::debug!(%tmdb_id, "catalog returned no metadata for Jellyfin item");
            return Ok(None);
        }
        Err(error) => {
            tracing::warn!(%error, %tmdb_id, "failed to load catalog metadata for Jellyfin item");
            return Err(error);
        }
    };
    // 旧口径：`season.zip(episode)`，两者都在才有分集。新载体只有整片与具体单元两态，
    // `UnitKey::episode_coordinates()` 把整片读成 `None`，非整片坐标由 `CHECK` 保证非负。
    let episode = (!snapshot.is_series && snapshot.media.kind == MediaKind::Tv)
        .then(|| snapshot.unit.and_then(|unit| unit.episode_coordinates()))
        .flatten()
        .and_then(|(season, episode)| catalog_episode(state, &tmdb_id, season, episode));
    Ok(Some(CatalogMerge { metadata, episode }))
}

impl ApiServerProvider {
    pub(super) async fn enrich_single_item(
        &self,
        snapshot: MediaItemSnapshot,
    ) -> MediaItemSnapshot {
        let state = self.state.clone();
        let fallback = snapshot.clone();
        let budget = state.http_work.clone();
        let task = budget
            .run_to_completion(move || {
                let mut snapshot = snapshot;
                enrich_snapshot_blocking(&state, &mut snapshot);
                snapshot
            })
            .await;
        match task {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "Jellyfin catalog metadata task failed");
                fallback
            }
        }
    }
}

/// 锁外：一次 probe 入队 + 一次 catalog 合并（单条目路径与选择路径共用）。
pub(super) fn enrich_snapshot_blocking(state: &ApiState, snapshot: &mut MediaItemSnapshot) {
    if !snapshot.is_series {
        // 旧路径交出的就是快照自身那一行 `(file, unit)`；新载体按同一事实构造覆盖：
        // 整片快照（`unit == None`）等价于「该文件没有 unit 行」，不是 `(0, 0)`。
        let coverage = domain::FileCoverage {
            path_key: String::new(),
            file: snapshot.file.clone(),
            units: snapshot.unit.into_iter().collect(),
        };
        crate::http::library::ensure_probe_enqueued_blocking(state, &coverage);
    }
    let Some(merge) = catalog_merge(state, snapshot) else {
        return;
    };
    let metadata = std::mem::take(&mut snapshot.metadata);
    snapshot.metadata = merge.apply(metadata, &snapshot.media);
}

fn catalog_episode(
    state: &ApiState,
    tmdb_lookup_id: &str,
    season: u32,
    episode: u32,
) -> Option<media::EpisodeMeta> {
    let language = crate::scrape_metadata::preferred_language(state);
    let translated = match state.catalog.cached_season_details_lang(
        tmdb_lookup_id,
        season,
        &language,
    ) {
        Ok(episodes) => episodes,
        Err(error) => {
            tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to read translated cached episode metadata");
            None
        }
    };
    let episodes = match translated {
        Some(episodes) if !episodes.is_empty() => episodes,
        _ => match state.catalog.cached_season_details(tmdb_lookup_id, season) {
            Ok(episodes) => episodes.unwrap_or_default(),
            Err(error) => {
                tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to read cached episode metadata");
                Vec::new()
            }
        },
    };
    episodes
        .into_iter()
        .find(|metadata| metadata.episode_number == episode)
}
