//! 轻候选：只有过滤/排序/计数需要的字段。
//!
//! 排序/过滤需要的 NFO 公共字段（genre/studio/person/search）只在查询真的用到时
//! 才读（线性成本，调用方显式记录）；tracks/chapters/NFO 细节留给选中的那一页。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use domain::{FileCoverage, LibraryFile, Media, MediaId, MediaKind};
use media_server::provider::{MediaItemMetadata, MediaItemSnapshot, ServerItemQuery};

use super::super::items::{
    UnitIndex, coverage_units, file_date_created, find_unit, snapshot_unit,
    unit_counts_from_snapshot, unit_index,
};
use super::scope::{ScopedRead, coverage_visible, in_scope_folder};

pub(super) struct LightItem {
    pub(super) snapshot: MediaItemSnapshot,
    /// 该条目的已发布 coverage：探测与页内重元数据必须看到同一份单元集合。
    pub(super) coverage: FileCoverage,
    /// series 折叠条目：作用域内可见的分集/季数。
    pub(super) child_counts: Option<(usize, usize)>,
    /// 非 series：DateCreated 用这条路径 stat；series：这些文件取 ingest 最大值。
    pub(super) stat_paths: Vec<PathBuf>,
    /// person 过滤用到的 catalog 载荷：本页重建 metadata 时按同一份载荷重放。
    pub(super) catalog: Option<super::super::enrichment::CatalogMerge>,
    /// 已为排序算出的 DateCreated，避免本页再 stat 一次。
    pub(super) date_created: Option<SystemTime>,
    /// 过滤时观察到的本行单元与（series）整片单元。重元数据必须用同一份，
    /// 不能再读一次 favourite，否则 `IsFavorite=true` 会返回 `IsFavorite=false`。
    pub(super) unit: Option<crate::store::PlaybackUnitSnapshot>,
    pub(super) whole_unit: Option<crate::store::PlaybackUnitSnapshot>,
}

/// 折叠/展开都在同一份候选上完成：folder 归属先定，再决定剧条目与分集条目。
pub(super) fn build_light_items(scope: &ScopedRead, query: &ServerItemQuery) -> Vec<LightItem> {
    let (show_episodes, show_seasons) = collect_show_units(scope);
    let (order, mut groups) = group_visible_coverages(scope);
    let context = LightContext {
        scope,
        units: unit_index(&scope.units),
        show_episodes: &show_episodes,
        show_seasons: &show_seasons,
        wants_episodes: query.wants_episodes(),
        wants_series: query.wants_series(),
    };
    let mut items = Vec::new();
    for media_id in order {
        let coverages = groups.remove(&media_id).unwrap_or_default();
        push_media_group(&context, &mut items, media_id, &coverages);
    }
    items
}

/// 轻候选的可见性索引与查询开关：逐 Media 出条目时反复使用。
struct LightContext<'a> {
    scope: &'a ScopedRead,
    units: UnitIndex<'a>,
    show_episodes: &'a HashMap<MediaId, HashSet<(u32, u32)>>,
    show_seasons: &'a HashMap<MediaId, HashSet<u32>>,
    wants_episodes: bool,
    wants_series: bool,
}

/// 作用域内可见剧集的「已出现季/集」索引。
///
/// 旧口径逐 `(file, unit)` 行：只要有一个具体单元就计入分集/季集合，整片不计。
fn collect_show_units(
    scope: &ScopedRead,
) -> (
    HashMap<MediaId, HashSet<(u32, u32)>>,
    HashMap<MediaId, HashSet<u32>>,
) {
    let mut show_episodes: HashMap<MediaId, HashSet<(u32, u32)>> = HashMap::new();
    let mut show_seasons: HashMap<MediaId, HashSet<u32>> = HashMap::new();
    for coverage in &scope.coverages {
        let Some(media) = scope.media.get(&coverage.file.media_id) else {
            continue;
        };
        if !scope.files.contains_key(&coverage.file.file_id) {
            continue;
        }
        if media.kind != MediaKind::Tv
            || !coverage_visible(scope, coverage)
            || !in_scope_folder(scope, coverage)
        {
            continue;
        }
        for unit in coverage.units.iter().filter(|unit| !unit.is_whole()) {
            show_episodes
                .entry(media.id)
                .or_default()
                .insert((unit.season as u32, unit.episode as u32));
            show_seasons.entry(media.id).or_default().insert(unit.season as u32);
        }
    }
    (show_episodes, show_seasons)
}

/// 可见 coverage 按 Media 分组，保留首次出现顺序。
fn group_visible_coverages<'a>(
    scope: &'a ScopedRead,
) -> (Vec<MediaId>, HashMap<MediaId, Vec<&'a FileCoverage>>) {
    let mut order: Vec<MediaId> = Vec::new();
    let mut groups: HashMap<MediaId, Vec<&FileCoverage>> = HashMap::new();
    for coverage in &scope.coverages {
        if !scope.files.contains_key(&coverage.file.file_id) {
            continue;
        }
        if !coverage_visible(scope, coverage) || !in_scope_folder(scope, coverage) {
            continue;
        }
        let group = groups.entry(coverage.file.media_id).or_default();
        if group.is_empty() {
            order.push(coverage.file.media_id);
        }
        group.push(coverage);
    }
    (order, groups)
}

/// 一个 Media 的候选：按需展开分集、折叠出 series 条目。
fn push_media_group(
    context: &LightContext<'_>,
    items: &mut Vec<LightItem>,
    media_id: MediaId,
    coverages: &[&FileCoverage],
) {
    let Some(media) = context.scope.media.get(&media_id).cloned() else {
        return;
    };
    let Some(first) = coverages.first().copied() else {
        return;
    };
    let series_parent_id = media.id.to_string().replace('-', "");
    let library_id = context
        .scope
        .ownership
        .owner(first.file.path.as_path(), media.kind)
        .map(|owner| owner.id.clone())
        .unwrap_or_else(|| series_parent_id.clone());
    if context.scope.media_scoped {
        // 读已经收敛到单个 Media：一个单元一条，不折叠（旧 `requested_series` 语义）。
        for &coverage in coverages {
            push_units(
                items,
                &context.units,
                context.show_episodes,
                context.scope,
                coverage,
                &media,
                false,
                None,
                &series_parent_id,
            );
        }
        return;
    }
    if media.kind != MediaKind::Tv {
        for &coverage in coverages {
            push_units(
                items,
                &context.units,
                context.show_episodes,
                context.scope,
                coverage,
                &media,
                false,
                None,
                &library_id,
            );
        }
        return;
    }
    if context.wants_episodes {
        for &coverage in coverages {
            push_units(
                items,
                &context.units,
                context.show_episodes,
                context.scope,
                coverage,
                &media,
                false,
                None,
                &series_parent_id,
            );
        }
    }
    if context.wants_series {
        let counts = (
            context
                .show_episodes
                .get(&media_id)
                .map(HashSet::len)
                .unwrap_or(0),
            context
                .show_seasons
                .get(&media_id)
                .map(HashSet::len)
                .unwrap_or(0),
        );
        let before = items.len();
        push_units(
            items,
            &context.units,
            context.show_episodes,
            context.scope,
            first,
            &media,
            true,
            Some(counts),
            &library_id,
        );
        if items.len() > before {
            // series 的 DateCreated 取该 Media 在作用域内可见 coverage 的 ingest 最大值。
            let all_paths = coverages
                .iter()
                .map(|coverage| coverage.file.path.clone())
                .collect();
            if let Some(item) = items.last_mut() {
                item.stat_paths = all_paths;
            }
        }
    }
}


/// 一个 coverage 的每个单元各出一条轻候选：旧台账里它们是同一文件的多行。
///
/// 单元集合由 [`coverage_units`] 给出（没有任何单元行 ⇒ 整片），因此候选的
/// `(file_id, unit)` 集合与换型前逐行读取完全一致。
#[allow(clippy::too_many_arguments)]
fn push_units(
    items: &mut Vec<LightItem>,
    units: &UnitIndex<'_>,
    show_episodes: &HashMap<MediaId, HashSet<(u32, u32)>>,
    scope: &ScopedRead,
    coverage: &FileCoverage,
    media: &Media,
    is_series: bool,
    child_counts: Option<(usize, usize)>,
    parent_id: &str,
) {
    let Some(file) = scope.files.get(&coverage.file.file_id).cloned() else {
        return;
    };
    for unit in coverage_units(coverage) {
        items.push(light_item(
            units,
            show_episodes,
            coverage,
            file.clone(),
            unit,
            media,
            is_series,
            child_counts,
            parent_id.to_string(),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn light_item(
    units: &UnitIndex<'_>,
    show_episodes: &HashMap<MediaId, HashSet<(u32, u32)>>,
    coverage: &FileCoverage,
    file: LibraryFile,
    unit_coords: (i32, i32),
    media: &Media,
    is_series: bool,
    child_counts: Option<(usize, usize)>,
    parent_id: String,
) -> LightItem {
    let (season, episode) = unit_coords;
    let media_id = coverage.file.media_id;
    let unit = find_unit(units, media_id, season, episode).cloned();
    let (played, play_count, unplayed_item_count) = unit_counts_from_snapshot(
        units,
        &coverage.file,
        unit_coords,
        media,
        is_series,
        show_episodes,
    );
    let whole_unit = is_series
        .then(|| {
            find_unit(
                units,
                media_id,
                crate::store::UNIT_WHOLE,
                crate::store::UNIT_WHOLE,
            )
            .cloned()
        })
        .flatten();
    let favorite_unit = if is_series {
        whole_unit.as_ref()
    } else {
        unit.as_ref()
    };
    LightItem {
        snapshot: MediaItemSnapshot {
            file,
            unit: snapshot_unit(unit_coords),
            media: media.clone(),
            ticks: if is_series {
                0
            } else {
                unit.as_ref()
                    .map(|unit| unit.state.position_ms.saturating_mul(10_000))
                    .unwrap_or(0)
            },
            play_count,
            played,
            unplayed_item_count,
            parent_id,
            is_series,
            chapters: Vec::new(),
            intro_start_ms: None,
            intro_end_ms: None,
            outro_start_ms: None,
            outro_end_ms: None,
            metadata: MediaItemMetadata {
                is_favorite: favorite_unit.is_some_and(|unit| unit.state.favorite),
                ..Default::default()
            },
        },
        coverage: coverage.clone(),
        child_counts,
        stat_paths: vec![coverage.file.path.clone()],
        catalog: None,
        date_created: None,
        unit,
        whole_unit,
    }
}

pub(super) fn needs_filter_facts(query: &ServerItemQuery, unknown_parent: bool) -> bool {
    unknown_parent
        || query
            .search_term
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .genres
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .genre_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .studio_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .person_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

pub(super) fn merge_filter_facts(
    mut metadata: MediaItemMetadata,
    facts: MediaItemMetadata,
) -> MediaItemMetadata {
    metadata.overview = facts.overview;
    metadata.genres = facts.genres;
    metadata.studios = facts.studios;
    metadata.original_title = facts.original_title;
    metadata.people = facts.people;
    metadata
}

pub(super) fn set_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    item: &mut LightItem,
) {
    let created = item_date_created(browse, item);
    item.date_created = created;
    item.snapshot.metadata.date_created = created.and_then(crate::compact_time::format_rfc3339_utc);
}

pub(super) fn item_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    item: &LightItem,
) -> Option<SystemTime> {
    paths_date_created(browse, item.snapshot.is_series, &item.stat_paths)
}

/// 非 series 取该文件的 stat 时间；series 取作用域内可见文件的 ingest 最大值
/// （与旧 `latest_ingest_by_media` 同口径：旧实现每行入账一次，取最大值，
/// 同一文件的多行对 `max` 无影响）。
pub(super) fn paths_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    is_series: bool,
    paths: &[PathBuf],
) -> Option<SystemTime> {
    if is_series {
        paths
            .iter()
            .filter_map(|path| file_date_created(browse, path.as_path()))
            .max()
    } else {
        paths
            .first()
            .and_then(|path| file_date_created(browse, path.as_path()))
    }
}
