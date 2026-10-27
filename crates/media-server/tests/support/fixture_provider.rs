//! 测试用 provider fixture：把 `list_visible_items` 的快照拼成协议页。
//!
//! 生产 provider 必须自行实现 `query_visible_items` / `next_up_items`；测试 fake
//! 只需要提供数据，选择语义由这里统一提供，避免每个测试各写一份过滤/分页。
//!
//! 多个测试目标用 `#[path]` 共用这个文件，而每个目标只用其中的一部分，
//! 所以在这里统一放行 `dead_code`，避免逐目标抖动（同 `maintenance_support`）。
#![allow(dead_code)]

use std::collections::HashSet;

use domain::{MediaKind, TypedItemId, UserId};
use media_server::provider::query::{
    ParentScope, classify_genre_or_studio, first_unwatched_per_media, item_matches_genre_or_studio,
    resolve_parent_scope, select_page,
};
use media_server::provider::{
    MediaItemSnapshot, MediaServerProvider, ServerItemPage, ServerItemQuery,
};

/// 测试 fixture 的条目选择：按 `list_visible_items` 取快照，再做纯过滤/排序/分页。
///
/// 这不是生产实现——生产 provider 必须实现
/// [`MediaServerProvider::query_visible_items`]。fixture 只求语义与协议一致：
/// folder 归属在折叠 series 之前确定、`SeriesId`/`SeasonId` 本身就是列分集的
/// 请求、任何读取失败都向上抛（绝不退化成空页）。
pub async fn select_items(
    provider: &(impl MediaServerProvider + ?Sized),
    user: UserId,
    query: ServerItemQuery,
) -> Result<ServerItemPage, String> {
    let libraries = provider.list_visible_libraries(user).await?;
    let parent = query.parent_id.as_deref();
    let series = query.series_id.as_deref();
    let scope = resolve_parent_scope(provider, user, parent, series, &libraries).await?;
    let is_folder_id = parent
        .map(|value| value.starts_with("folder_"))
        .unwrap_or(false);
    if is_folder_id && !matches!(scope, ParentScope::Folder(_)) {
        // 认不出的 folder 既不是可见库根、也不是 genre/studio：空集合，绝不整库。
        return Ok(ServerItemPage::default());
    }
    let needs_items = matches!(scope, ParentScope::Unknown);
    let parent_id_for_provider = match &scope {
        ParentScope::Library(id) | ParentScope::Series(id) => Some(id.as_str()),
        _ => None,
    };
    let mut rows = provider
        .list_visible_items(user, parent_id_for_provider)
        .await?;
    let wants_episodes = query.wants_episodes();
    let wants_series = query.wants_series();
    // folder 归属必须先于折叠：代表行在别的根下不能决定整个 series 是否属于
    // 这个 folder。先取未折叠的分集，再用 folder 内的行重建剧快照。
    if let ParentScope::Folder(root) = &scope {
        if rows.iter().any(|row| row.is_series) {
            let mut expanded = Vec::new();
            for item in rows {
                if item.is_series {
                    let series_id = TypedItemId::Work(item.media.id).to_wire_string();
                    let episodes = provider.list_visible_items(user, Some(&series_id)).await?;
                    let in_folder: Vec<MediaItemSnapshot> = episodes
                        .into_iter()
                        .filter(|episode| episode.file.path.starts_with(root))
                        .collect();
                    if wants_series && let Some(representative) = in_folder.first() {
                        let mut series = item;
                        series.unit = representative.unit;
                        series.file = representative.file.clone();
                        series.metadata.child_count = Some(in_folder.len());
                        series.metadata.season_count = Some(
                            in_folder
                                .iter()
                                .filter_map(|episode| episode.unit.map(|u| u.season))
                                .collect::<HashSet<_>>()
                                .len(),
                        );
                        series.unplayed_item_count =
                            Some(in_folder.iter().filter(|episode| !episode.played).count());
                        series.played = in_folder.iter().all(|episode| episode.played);
                        expanded.push(series);
                    }
                    if wants_episodes {
                        expanded.extend(in_folder);
                    }
                } else if item.file.path.starts_with(root)
                    && (item.media.kind != MediaKind::Tv
                        || item.is_series && wants_series
                        || !item.is_series && wants_episodes)
                {
                    expanded.push(item);
                }
            }
            rows = expanded;
        } else {
            rows.retain(|item| {
                item.file.path.starts_with(root)
                    && (item.is_series && wants_series || !item.is_series && wants_episodes)
            });
        }
    } else if wants_episodes && rows.iter().any(|row| row.is_series) {
        let mut expanded = Vec::new();
        for item in rows {
            if item.is_series {
                let series_id = TypedItemId::Work(item.media.id).to_wire_string();
                let episodes = match &scope {
                    ParentScope::Library(library_id) => {
                        provider
                            .list_visible_items_in_library(user, Some(&series_id), library_id)
                            .await?
                    }
                    _ => provider.list_visible_items(user, Some(&series_id)).await?,
                };
                if wants_episodes {
                    expanded.extend(episodes);
                }
                if wants_series {
                    expanded.push(item);
                }
            } else if item.is_series && wants_series || !item.is_series && wants_episodes {
                expanded.push(item);
            }
        }
        rows = expanded;
    }
    let genre_target = if needs_items {
        match classify_genre_or_studio(parent.unwrap_or_default(), &rows) {
            ParentScope::Genre(id) | ParentScope::Studio(id) => Some(id),
            _ => None,
        }
    } else {
        None
    };
    if let Some(target_id) = genre_target {
        rows.retain(|item| {
            item_matches_genre_or_studio(
                &target_id,
                item.metadata.genres.as_slice(),
                item.metadata.studios.as_slice(),
            )
        });
    } else if needs_items {
        return Ok(ServerItemPage::default());
    }
    if query.person_ids.is_some() {
        let mut enriched = Vec::with_capacity(rows.len());
        for row in rows {
            enriched.push(provider.enrich_visible_item(row).await);
        }
        rows = enriched;
    }
    Ok(select_page(rows, &query))
}

/// 测试 fixture 的 NextUp：一次候选读 + 按 Media 分组选第一条未看。
///
/// 根列表只有 series 级快照时退回逐 series 查询，因此不承诺有界；生产 provider
/// 自行实现 [`MediaServerProvider::next_up_items`] 并一次聚合。
pub async fn next_up_items(
    provider: &(impl MediaServerProvider + ?Sized),
    user: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let requested = series_id.map(str::to_string);
    let candidates = provider
        .list_visible_items(user, requested.as_deref())
        .await?;
    // 显式 SeriesId 与生产路径一致：不限制种类（电影也能返回）。缺省才只要 TV。
    let restrict_kind = series_id.is_none();
    let mut episodes = Vec::new();
    let mut expanded_media = HashSet::new();
    for item in candidates {
        if !item.is_series {
            if !restrict_kind || item.media.kind == MediaKind::Tv {
                expanded_media.insert(item.media.id);
                episodes.push(item);
            }
            continue;
        }
        if !expanded_media.insert(item.media.id) {
            continue;
        }
        let series_id = TypedItemId::Work(item.media.id).to_wire_string();
        let expanded = provider.list_visible_items(user, Some(&series_id)).await?;
        episodes.extend(expanded.into_iter().filter(|episode| {
            !episode.is_series && (!restrict_kind || episode.media.kind == MediaKind::Tv)
        }));
    }
    Ok(first_unwatched_per_media(episodes, offset, limit))
}
