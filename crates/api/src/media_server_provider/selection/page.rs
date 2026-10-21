//! 协议查询 → `ProtocolItemQuery` 的逐字段适配：筛选、去重、排序、计数、取页都在
//! store 的一次短读事务里完成，这里只负责把合同字段翻过去。
//!
//! 父级归属**不在这里重新推导**：库/剧/分集/folder 的收敛由 `read_scope` 决定，
//! 适配只把已确认的作用域翻成协议父级，所以不存在「同一份 parent 文本两套语义」。
//! 认不出的父级先按流派探测、再按工作室，都不命中就是空页，绝不退化成整库。

use std::str::FromStr;

use domain::{
    LibraryId, MediaId, MediaKind, ProtocolItemPage, ProtocolItemQuery, ProtocolItemType,
    ProtocolMarkFilter, ProtocolParent, ProtocolSort, ProtocolSortKey, TypedItemId,
};
use media_server::provider::ServerItemQuery;
use store::Store;

use super::scope::ScopedRead;

/// 选页结论：完整的一页（含分页前总数），或「这次请求不可能命中」的空页。
pub(super) enum PageSelection {
    Selected(ProtocolItemPage),
    Empty,
}

/// 在 store 锁内为这次请求选出完整的一页。
pub(super) fn select_page(
    store: &Store,
    scope: &ScopedRead,
    query: &ServerItemQuery,
) -> Result<PageSelection, String> {
    if scope.unknown_parent {
        return probe_unknown_parent(store, scope, query);
    }
    let Some(parent) = protocol_parent(store, scope, query) else {
        return Ok(PageSelection::Empty);
    };
    let Some(request) = protocol_query(scope, query, parent) else {
        return Ok(PageSelection::Empty);
    };
    let page = run_page(store, &request)?;
    Ok(PageSelection::Selected(page))
}

/// 认不出的父级：流派优先、工作室兜底，都不命中就是空页（与旧分类同序）。
fn probe_unknown_parent(
    store: &Store,
    scope: &ScopedRead,
    query: &ServerItemQuery,
) -> Result<PageSelection, String> {
    let parent = query.parent_id.clone().unwrap_or_default();
    for candidate in [
        ProtocolParent::Genre(parent.clone()),
        ProtocolParent::Studio(parent),
    ] {
        let Some(request) = protocol_query(scope, query, candidate) else {
            continue;
        };
        let page = run_page(store, &request)?;
        if page.total > 0 {
            return Ok(PageSelection::Selected(page));
        }
    }
    Ok(PageSelection::Empty)
}

fn run_page(store: &Store, request: &ProtocolItemQuery) -> Result<ProtocolItemPage, String> {
    let page = store.protocol_item_page(request).map_err(|error| {
        tracing::error!(error = %error, "Jellyfin 协议选页失败");
        error.to_string()
    })?;
    tracing::debug!(
        total = page.total,
        returned = page.items.len(),
        "Jellyfin 协议选页完成"
    );
    Ok(page)
}

/// 作用域已确认的父级：folder → 库 → 剧 → 全库。
fn protocol_parent(
    store: &Store,
    scope: &ScopedRead,
    query: &ServerItemQuery,
) -> Option<ProtocolParent> {
    if let Some(root) = scope.folder_root.as_deref() {
        let root_id = store
            .protocol_folder_root_id(root.to_str()?)
            .ok()
            .flatten()?;
        return Some(ProtocolParent::Folder { root_id });
    }
    if let Some(library) = scope.requested_library.as_deref() {
        return LibraryId::from_str(library)
            .ok()
            .map(ProtocolParent::Library);
    }
    if let Some(media) = scoped_media(scope, query) {
        return Some(ProtocolParent::Series(media));
    }
    Some(ProtocolParent::Root)
}

/// `read_scope` 收敛出的那部媒体（与它同源，不复用 parent 文本）。
fn scoped_media(scope: &ScopedRead, query: &ServerItemQuery) -> Option<MediaId> {
    if !scope.media_scoped {
        return None;
    }
    let explicit = if scope.requested_library.is_some() || scope.folder_root.is_some() {
        query.series_id.as_deref()
    } else {
        query.parent_id.as_deref().or(query.series_id.as_deref())
    };
    explicit.and_then(TypedItemId::protocol_media_id)
}

/// 合同字段 → `ProtocolItemQuery`；`None` 表示这次请求在这一作用域下必为空页。
fn protocol_query(
    scope: &ScopedRead,
    query: &ServerItemQuery,
    parent: ProtocolParent,
) -> Option<ProtocolItemQuery> {
    let media = scoped_media(scope, query);
    if scope.media_scoped && media.is_none() {
        return None;
    }
    let season = season_pin(scope, query, media)?;
    let pinned_kind = media
        .and_then(|id| scope.media.get(&id))
        .map(|item| item.kind);
    let episode_only =
        season.is_some() || (scope.media_scoped && pinned_kind == Some(MediaKind::Tv));
    let include_types = include_types(
        query.include_item_types.as_deref(),
        scope.media_scoped,
        episode_only,
    )?;
    // 剧级父级已隐含钉住媒体；库/folder 父级要靠 `series_id` 才能把范围收到这部剧。
    let series_id = media.filter(|_| !matches!(parent, ProtocolParent::Series(_)));
    Some(ProtocolItemQuery {
        user_id: scope.user_id,
        parent,
        series_id,
        season,
        person_ids: Vec::new(),
        studio_ids: values(query.studio_ids.as_deref()),
        search_term: query
            .search_term
            .clone()
            .map(|term| term.trim().to_string()),
        genres: values(query.genres.as_deref()),
        genre_ids: values(query.genre_ids.as_deref()),
        filters: mark_filters(query.filters.as_deref()),
        is_favorite: query.is_favorite,
        is_played: query.is_played,
        include_types,
        recursive: query.recursive,
        is_folder: query.is_folder,
        sort: sorts(query, episode_only),
        offset: query.offset,
        limit: query.limit,
    })
}

/// `SeasonId` 只在它命中的那部媒体上生效；拼不出来的 `SeasonId` 一律空页。
fn season_pin(
    scope: &ScopedRead,
    query: &ServerItemQuery,
    media: Option<MediaId>,
) -> Option<Option<(MediaId, u32)>> {
    let Some(raw) = query.season_id.as_deref() else {
        return Some(None);
    };
    match TypedItemId::from_wire_string(raw) {
        Some(TypedItemId::Season {
            media: season_media,
            season,
        }) => match (media, scope.media_scoped) {
            (Some(pinned), _) if pinned != season_media => None,
            (Some(_), _) => Some(Some((season_media, season))),
            // 只有 `SeasonId`、没有 `SeriesId`/父级时，季号自己就是作用域。
            (None, false) => Some(Some((season_media, season))),
            (None, true) => None,
        },
        _ => None,
    }
}

/// `IncludeItemTypes` 与候选形态：缺省列表等价于「剧折叠成一条 + 影片」，
/// 钉住剧/季时分集才是候选；`Season`/`Folder` 这类没有候选形态的类型保持空页。
fn include_types(
    raw: Option<&str>,
    media_scoped: bool,
    episode_only: bool,
) -> Option<Vec<ProtocolItemType>> {
    match raw {
        None => Some(if episode_only {
            vec![ProtocolItemType::Episode]
        } else {
            vec![
                ProtocolItemType::Movie,
                ProtocolItemType::Series,
                ProtocolItemType::Video,
            ]
        }),
        Some(raw) => {
            let tokens = item_types(raw);
            if episode_only {
                return tokens
                    .contains(&ProtocolItemType::Episode)
                    .then(|| vec![ProtocolItemType::Episode]);
            }
            if media_scoped {
                let keep: Vec<ProtocolItemType> = tokens
                    .iter()
                    .copied()
                    .filter(|kind| *kind != ProtocolItemType::Episode)
                    .collect();
                return (!keep.is_empty()).then_some(keep);
            }
            (!tokens.is_empty()).then_some(tokens)
        }
    }
}

/// 逗号分隔的类型名；`Season`/`Folder`/未知名字没有候选形态，直接丢掉。
fn item_types(raw: &str) -> Vec<ProtocolItemType> {
    let mut types = Vec::new();
    for token in raw.split(',') {
        let kind = match token.trim().to_ascii_lowercase().as_str() {
            "movie" => ProtocolItemType::Movie,
            "series" => ProtocolItemType::Series,
            "episode" => ProtocolItemType::Episode,
            "video" => ProtocolItemType::Video,
            _ => continue,
        };
        if !types.contains(&kind) {
            types.push(kind);
        }
    }
    types
}

/// `Filters`：旧语义是全部命中（`IsFavorite`/`IsPlayed`/`IsUnplayed`）。
fn mark_filters(raw: Option<&str>) -> Vec<ProtocolMarkFilter> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut filters = Vec::new();
    for token in raw.split(',') {
        let filter = match token.trim().to_ascii_lowercase().as_str() {
            "isfavorite" => ProtocolMarkFilter::Favorite,
            "isplayed" => ProtocolMarkFilter::Played,
            "isunplayed" => ProtocolMarkFilter::Unplayed,
            _ => continue,
        };
        if !filters.contains(&filter) {
            filters.push(filter);
        }
    }
    filters
}

/// 逗号/竖线分隔的多值字段（旧实现两种分隔都认）。
fn values(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    raw.split([',', '|'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// 排序键与方向；缺省沿用旧默认（分集页按季/集升序，其余按创建时间降序）。
fn sorts(query: &ServerItemQuery, episode_only: bool) -> Vec<ProtocolSort> {
    let Some(sort_by) = query.sort_by.as_deref() else {
        return default_sorts(episode_only);
    };
    let orders: Vec<&str> = query
        .sort_order
        .as_deref()
        .map(|order| order.split(',').collect())
        .unwrap_or_default();
    let last = orders.last().copied();
    sort_by
        .split(',')
        .enumerate()
        .map(|(index, key)| {
            let order = orders.get(index).copied().or(last).unwrap_or("Descending");
            ProtocolSort {
                key: sort_key(key),
                descending: !order.trim().eq_ignore_ascii_case("ascending"),
            }
        })
        .collect()
}

fn default_sorts(episode_only: bool) -> Vec<ProtocolSort> {
    let keys = if episode_only {
        [
            ProtocolSortKey::SeasonNumber,
            ProtocolSortKey::EpisodeNumber,
        ]
    } else {
        [ProtocolSortKey::DateCreated, ProtocolSortKey::Name]
    };
    keys.into_iter()
        .map(|key| ProtocolSort {
            key,
            descending: !episode_only,
        })
        .collect()
}

/// 认不出的排序键按旧实现当「相等」，只影响同组内的并列顺序。
fn sort_key(key: &str) -> ProtocolSortKey {
    match key.trim().to_ascii_lowercase().as_str() {
        "sortname" | "name" => ProtocolSortKey::Name,
        "datecreated" => ProtocolSortKey::DateCreated,
        "parentindexnumber" => ProtocolSortKey::SeasonNumber,
        "indexnumber" => ProtocolSortKey::EpisodeNumber,
        _ => ProtocolSortKey::Noop,
    }
}
