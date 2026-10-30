use super::user_marks::{item_is_played, unit_play_count};
use super::*;
use std::time::SystemTime;

mod ingest_time;
pub(super) use ingest_time::{file_date_created, latest_ingest_by_media, with_date_created};

/// A public protocol id resolved to the entry it names.
pub(super) struct ResolvedEntry {
    pub file: domain::LibraryFile,
    pub media: domain::Media,
    /// 该条目覆盖的观看单元；`(-1, -1)` 表示整个作品。
    pub unit: (i32, i32),
    pub is_series: bool,
}

/// 整片单元：旧行模型用 `season: None, episode: None` 表达的同一件事。
pub(super) fn is_whole_unit(unit: (i32, i32)) -> bool {
    unit.0 < 0 || unit.1 < 0
}

/// 单元坐标按 DB 访问器要求的可空列形式给出；整片是 `(None, None)`。
pub(crate) fn unit_columns(unit: (i32, i32)) -> (Option<u32>, Option<u32>) {
    if is_whole_unit(unit) {
        (None, None)
    } else {
        (Some(unit.0 as u32), Some(unit.1 as u32))
    }
}

/// 单元按 media-server 快照的形状给出：整片没有坐标，是 `None`，绝不拼 `(0, 0)`。
pub(super) fn snapshot_unit(unit: (i32, i32)) -> Option<domain::UnitKey> {
    let coordinate = |value: i32| (value >= 0).then_some(value);
    match (coordinate(unit.0), coordinate(unit.1)) {
        (Some(season), Some(episode)) => Some(domain::UnitKey::new(season, episode)),
        _ => None,
    }
}

/// [`snapshot_unit`] 的逆向：协议快照里的 `None` 还原成整片坐标 `(-1, -1)`。
pub(super) fn snapshot_unit_coords(unit: Option<domain::UnitKey>) -> (i32, i32) {
    match unit {
        Some(unit) => (unit.season, unit.episode),
        None => (
            domain::UnitKey::WHOLE.season,
            domain::UnitKey::WHOLE.episode,
        ),
    }
}

/// 一个 coverage 显式记录的全部播放单元坐标。
///
/// 无兼容切换：没有任何 unit 行的文件是“未知覆盖”，不生成任何逻辑可播身份，
/// 也不能被当成整片（work）借用人物图片等元数据。
#[cfg(test)]
pub(super) fn coverage_units(coverage: &domain::FileCoverage) -> Vec<(i32, i32)> {
    coverage
        .units
        .iter()
        .map(|unit| (unit.season, unit.episode))
        .collect()
}

/// Resolve a public item id (`w1_…` work, `s1_…` season, `u1_…` unit) inside the
/// caller's authorized libraries. A pending, missing or unknown source never
/// resolves, and a physical file id is not an item id.
pub(super) fn resolve_entry(
    store: &crate::Store,
    user_id: Option<UserId>,
    id: &str,
) -> Result<Option<ResolvedEntry>, String> {
    let Some(item) = domain::TypedItemId::from_wire_string(id) else {
        return Ok(None);
    };
    let selected = match user_id {
        Some(user_id) => store.playable_entries(user_id, &item),
        // Public artwork routes resolve an item id before any user is known.
        None => store.published_entries_for_item(&item),
    }
    .map_err(|error| error.to_string())?;
    let Some(source) = selected.into_iter().next() else {
        return Ok(None);
    };
    let Some(media) = store
        .get_media(source.file.media_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let is_series = media.kind == MediaKind::Tv && matches!(item, domain::TypedItemId::Work(_));
    let unit = (source.unit.season, source.unit.episode);
    Ok(Some(ResolvedEntry {
        file: source.file,
        media,
        unit,
        is_series,
    }))
}

/// Falls back to a full snapshot for the caller-resolved entry; `None` when the
/// entry is not a published, present source.
pub(super) fn entry_snapshot(
    state: &ApiState,
    user_id: UserId,
    id: &str,
) -> Result<Option<MediaItemSnapshot>, String> {
    let entry =
        super::sync_operations::read_store(state, |store| resolve_entry(store, Some(user_id), id))?;
    entry
        .map(|entry| {
            snapshot_from_entry(
                state,
                user_id,
                entry.file,
                entry.unit,
                entry.media,
                entry.is_series,
            )
        })
        .transpose()
}

pub(super) fn snapshot_from_entry(
    state: &ApiState,
    user_id: UserId,
    file: domain::LibraryFile,
    unit: (i32, i32),
    media: domain::Media,
    is_series: bool,
) -> Result<MediaItemSnapshot, String> {
    // 读 scope 内只取 owned DB 事实，每个 SQL 使用对应数据库的 reader。
    let (marker, chapters, ticks, played, play_count, parent_id, facts, fields, series_counts) =
        super::sync_operations::read_store(state, |store| {
            let (season, episode) = unit_columns(unit);
            let marker = store
                .get_media_marker(file.media_id, season, episode)
                .ok()
                .flatten();
            let chapters = store
                .get_cached_chapters(&file.file_id.to_string())
                .ok()
                .flatten()
                .unwrap_or_default();
            let ticks = playback_ticks(store, user_id, &file, unit, &media);
            let is_episode = media.kind == MediaKind::Tv && !is_whole_unit(unit);
            let parent_id = if is_episode {
                domain::TypedItemId::Work(media.id).to_wire_string()
            } else {
                crate::http::playback::library_id_for(store, &media, &[&file])
                    .unwrap_or_else(|| domain::TypedItemId::Work(media.id).to_wire_string())
            };
            let (played, play_count, series_counts) = if is_series {
                let (played, unplayed, child_counts) =
                    direct_series_rollup(store, user_id, media.id);
                (played, 0, Some((unplayed, child_counts)))
            } else {
                let played = item_is_played(store, user_id, &file, unit);
                let play_count = unit_play_count(store, user_id, file.media_id, unit.0, unit.1);
                (played, play_count, None)
            };
            let (own_unit, whole_unit) =
                item_metadata::row_units(store, user_id, &file, unit, is_series);
            let facts = item_metadata::item_db_facts(
                store,
                &file,
                unit,
                is_series,
                own_unit.as_ref(),
                whole_unit.as_ref(),
            );
            let fields = item_metadata::page_fields(store, &file, unit, &media, is_series)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, media_id = %file.media_id, "page metadata 读取失败，按空字段渲染");
                domain::MetadataFields::default()
            });
            Ok((
                marker,
                chapters,
                ticks,
                played,
                play_count,
                parent_id,
                facts,
                fields,
                series_counts,
            ))
        })?;
    // reader 已释放：stat / 外部字幕 IO。
    let metadata = with_date_created(
        item_metadata(
            facts,
            &fields,
            state.file_stats.as_ref(),
            &file,
            unit,
            &media,
            is_series,
            series_counts.map(|(_, counts)| counts),
        ),
        file_date_created(state.file_stats.as_ref(), &file.path),
    );
    Ok(MediaItemSnapshot {
        file,
        unit: snapshot_unit(unit),
        played,
        play_count,
        unplayed_item_count: series_counts.map(|(unplayed, _)| unplayed),
        metadata,
        media,
        ticks,
        parent_id,
        is_series,
        chapters,
        intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
    })
}

fn direct_series_rollup(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
) -> (bool, usize, (usize, usize)) {
    let Ok(snapshot) = super::read_snapshot::collect_server_read(store, user_id, None, None) else {
        return (false, 0, (0, 0));
    };
    let mut episodes = HashSet::new();
    let mut seasons = HashSet::new();
    for coverage in &snapshot.coverages {
        if coverage.file.media_id != media_id {
            continue;
        }
        let Some(media) = snapshot.media.get(&media_id) else {
            continue;
        };
        // Published ownership is the row's explicit Library, never a path guess.
        let Some(owner) = snapshot
            .ownership
            .library(&coverage.file.library_id.to_string())
        else {
            continue;
        };
        let accessible = snapshot.ownership.accepts_work(&owner.id)
            && if snapshot.is_admin {
                owner.admin_visible
            } else {
                owner.access_mode == "everyone" || owner.member_ids.contains(&user_id)
            };
        if accessible && media.kind == MediaKind::Tv {
            for unit in coverage.units.iter().filter(|unit| !unit.is_whole()) {
                episodes.insert((unit.season as u32, unit.episode as u32));
                seasons.insert(unit.season as u32);
            }
        }
    }
    let units = unit_index(&snapshot.units);
    let played_count = episodes
        .iter()
        .filter(|(season, episode)| {
            effective_played(&units, media_id, *season as i32, *episode as i32)
        })
        .count();
    let total = episodes.len();
    (
        total == 0 || played_count == total,
        total.saturating_sub(played_count),
        (total, seasons.len()),
    )
}

pub(super) async fn list_visible_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    parent_id: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    list_visible_items_with_library_scope(provider, user_id, parent_id, None).await
}

pub(super) async fn list_visible_items_in_library(
    provider: &ApiServerProvider,
    user_id: UserId,
    parent_id: Option<&str>,
    library_id: &str,
) -> Result<Vec<MediaItemSnapshot>, String> {
    list_visible_items_with_library_scope(provider, user_id, parent_id, Some(library_id)).await
}

async fn list_visible_items_with_library_scope(
    provider: &ApiServerProvider,
    user_id: UserId,
    parent_id: Option<&str>,
    library_scope: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    let parent_id = parent_id.map(str::to_owned);
    let library_scope = library_scope.map(str::to_owned);
    budget
        .run(std::time::Duration::from_secs(30), move || {
            list_visible_items_blocking(&state, user_id, parent_id.as_deref(), library_scope.as_deref())
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin item listing blocking work failed");
            error.to_string()
        })?
}

fn list_visible_items_blocking(
    state: &ApiState,
    user_id: UserId,
    parent_id: Option<&str>,
    library_scope: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    let snapshot = super::sync_operations::read_store(state, |store| {
        collect_pending(store, user_id, parent_id, library_scope)
    })?;
    let (pending, ingest_paths) = snapshot;
    let browse = state.file_stats.as_ref();
    let latest_ingest = latest_ingest_by_media(browse, &ingest_paths);
    let mut visible = Vec::with_capacity(pending.len());
    for item in pending {
        let date_created = match &item.stat_path {
            Some(path) => file_date_created(browse, std::path::Path::new(path)),
            None => latest_ingest.get(&item.media.id).copied(),
        };
        visible.push(item.into_snapshot(browse, date_created));
    }
    Ok(visible)
}

pub(super) type UnitIndex<'a> =
    HashMap<(domain::MediaId, i32, i32), &'a crate::store::PlaybackUnitSnapshot>;

/// 一次索引：`find_unit` 的线性扫描在剧集聚合里是 O(集数 × 单元数)。
pub(super) fn unit_index(units: &[crate::store::PlaybackUnitSnapshot]) -> UnitIndex<'_> {
    units
        .iter()
        .map(|unit| ((unit.media_id, unit.season, unit.episode), unit))
        .collect()
}

pub(super) fn find_unit<'a>(
    units: &UnitIndex<'a>,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Option<&'a crate::store::PlaybackUnitSnapshot> {
    units.get(&(media_id, season, episode)).copied()
}

pub(super) fn effective_played(
    units: &UnitIndex<'_>,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    super::user_marks::resolve_effective_played(
        find_unit(units, media_id, season, episode)
            .map(|unit| (&unit.state, unit.state.updated_at)),
        find_unit(units, media_id, season, crate::store::UNIT_WHOLE)
            .map(|unit| (&unit.state, unit.state.updated_at)),
        find_unit(
            units,
            media_id,
            crate::store::UNIT_WHOLE,
            crate::store::UNIT_WHOLE,
        )
        .map(|unit| (&unit.state, unit.state.updated_at)),
    )
}
pub(super) fn unit_counts_from_snapshot(
    units: &UnitIndex<'_>,
    file: &domain::LibraryFile,
    unit: (i32, i32),
    media: &domain::Media,
    is_series: bool,
    show_episodes: &HashMap<domain::MediaId, HashSet<(u32, u32)>>,
) -> (bool, i64, Option<usize>) {
    if is_series {
        let episodes = show_episodes.get(&media.id);
        let total = episodes.map(HashSet::len).unwrap_or(0);
        let played = episodes
            .into_iter()
            .flat_map(|episodes| episodes.iter())
            .filter(|(season, episode)| {
                effective_played(units, media.id, *season as i32, *episode as i32)
            })
            .count();
        return (
            total == 0 || total == played,
            0,
            Some(total.saturating_sub(played)),
        );
    }
    let (season, episode) = unit;
    let own_unit = find_unit(units, file.media_id, season, episode);
    let played = if media.kind == MediaKind::Tv && season >= 0 && episode >= 0 {
        effective_played(units, file.media_id, season, episode)
    } else {
        own_unit.is_some_and(|unit| unit.state.played)
    };
    (
        played,
        own_unit.map(|unit| unit.state.play_count).unwrap_or(0),
        None,
    )
}

fn collect_pending(
    store: &crate::Store,
    user_id: UserId,
    parent_id: Option<&str>,
    library_scope: Option<&str>,
) -> Result<(Vec<PendingSnapshot>, Vec<(domain::MediaId, String)>), String> {
    let snapshot =
        super::read_snapshot::collect_server_read(store, user_id, parent_id, library_scope)?;
    let ServerReadSnapshot {
        ownership,
        is_admin,
        media,
        coverages,
        units,
        files,
    } = snapshot;
    let requested_library = library_scope.or_else(|| {
        parent_id
            .and_then(|id| ownership.library(id))
            .map(|library| library.id.as_str())
    });
    let mut show_episodes: HashMap<domain::MediaId, HashSet<(u32, u32)>> = HashMap::new();
    let mut show_seasons: HashMap<domain::MediaId, HashSet<u32>> = HashMap::new();
    let coverage_visible = |coverage: &domain::FileCoverage| -> bool {
        let Some(owner) = ownership.library(&coverage.file.library_id.to_string()) else {
            return false;
        };
        media.contains_key(&coverage.file.media_id)
            && ownership.accepts_work(&owner.id)
            && requested_library.is_none_or(|library_id| owner.id == library_id)
            && if is_admin {
                owner.admin_visible
            } else {
                owner.access_mode == "everyone" || owner.member_ids.contains(&user_id)
            }
    };
    collect_show_units(
        &coverages,
        &media,
        &coverage_visible,
        &mut show_episodes,
        &mut show_seasons,
    );
    let units = unit_index(&units);
    let ingest_paths = coverages
        .iter()
        .filter_map(|coverage| {
            let item = media.get(&coverage.file.media_id)?;
            coverage_visible(coverage).then(|| (item.id, coverage.file.path.display().to_string()))
        })
        .collect::<Vec<_>>();
    let context = PendingContext {
        store,
        units: &units,
        show_episodes: &show_episodes,
        show_seasons: &show_seasons,
    };
    let mut seen_series = HashSet::new();
    let mut pending = Vec::new();
    for coverage in &coverages {
        // Published, present sources only: a pending or missing file is not
        // library content.
        let Some(file) = files.get(&coverage.file.file_id).cloned() else {
            continue;
        };
        let Some(media) = media.get(&coverage.file.media_id).cloned() else {
            continue;
        };
        let Some(owner) = ownership.library(&coverage.file.library_id.to_string()) else {
            continue;
        };
        if !coverage_visible(coverage) {
            continue;
        }
        let series_id = domain::TypedItemId::Work(media.id).to_wire_string();
        let requested_series = media.kind == MediaKind::Tv
            && parent_id.is_some_and(|id| {
                media_server::provider::query::series_media_id(id) == Some(media.id)
            });
        let is_series = media.kind == MediaKind::Tv && !requested_series;
        for watch_unit in &coverage.units {
            if is_series && !seen_series.insert(media.id) {
                continue;
            }
            let unit = (watch_unit.season, watch_unit.episode);
            pending.push(pending_for_unit(
                &context,
                coverage,
                &file,
                unit,
                &media,
                is_series,
                requested_series,
                &series_id,
                &owner.id,
            )?);
        }
    }
    Ok((pending, ingest_paths))
}

/// 可见剧集的「已出现季/集」索引：折叠 series 条目时用它算 child counts。
fn collect_show_units(
    coverages: &[domain::FileCoverage],
    media: &HashMap<domain::MediaId, domain::Media>,
    visible: &impl Fn(&domain::FileCoverage) -> bool,
    show_episodes: &mut HashMap<domain::MediaId, HashSet<(u32, u32)>>,
    show_seasons: &mut HashMap<domain::MediaId, HashSet<u32>>,
) {
    for coverage in coverages {
        let Some(item) = media.get(&coverage.file.media_id) else {
            continue;
        };
        if item.kind != MediaKind::Tv || !visible(coverage) {
            continue;
        }
        for unit in coverage.units.iter().filter(|unit| !unit.is_whole()) {
            show_episodes
                .entry(item.id)
                .or_default()
                .insert((unit.season as u32, unit.episode as u32));
            show_seasons
                .entry(item.id)
                .or_default()
                .insert(unit.season as u32);
        }
    }
}

/// 单个 `(文件, 观看单元)` 条目所需的 DB 事实与渲染字段。
#[allow(clippy::too_many_arguments)]
fn pending_for_unit(
    context: &PendingContext<'_>,
    coverage: &domain::FileCoverage,
    file: &domain::LibraryFile,
    unit: (i32, i32),
    media: &domain::Media,
    is_series: bool,
    requested_series: bool,
    series_id: &str,
    owner_id: &str,
) -> Result<PendingSnapshot, String> {
    let (season, episode) = unit_columns(unit);
    let marker = context
        .store
        .get_media_marker(coverage.file.media_id, season, episode)
        .map_err(|error| error.to_string())?;
    let chapters = context
        .store
        .get_cached_chapters(&coverage.file.file_id.to_string())
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let (played, play_count, unplayed_item_count) = unit_counts_from_snapshot(
        context.units,
        &coverage.file,
        unit,
        media,
        is_series,
        context.show_episodes,
    );
    let child_counts = is_series.then(|| {
        (
            context
                .show_episodes
                .get(&media.id)
                .map(HashSet::len)
                .unwrap_or(0),
            context
                .show_seasons
                .get(&media.id)
                .map(HashSet::len)
                .unwrap_or(0),
        )
    });
    let own_unit = find_unit(context.units, coverage.file.media_id, unit.0, unit.1);
    let whole = crate::store::UNIT_WHOLE;
    let whole_unit = is_series
        .then(|| find_unit(context.units, coverage.file.media_id, whole, whole))
        .flatten();
    let facts = item_metadata::item_db_facts(
        context.store,
        &coverage.file,
        unit,
        is_series,
        own_unit,
        whole_unit,
    );
    let fields = item_metadata::page_fields(context.store, &coverage.file, unit, media, is_series)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, media_id = %coverage.file.media_id, "page metadata 读取失败，按空字段渲染");
            domain::MetadataFields::default()
        });
    Ok(PendingSnapshot {
        parent_id: if requested_series {
            series_id.to_string()
        } else {
            owner_id.to_string()
        },
        ticks: own_unit
            .map(|unit| unit.state.position_ms.saturating_mul(10_000))
            .unwrap_or(0),
        intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
        file: file.clone(),
        unit,
        media: media.clone(),
        is_series,
        play_count,
        played,
        unplayed_item_count,
        chapters,
        facts,
        fields,
        child_counts,
        stat_path: (!is_series).then(|| coverage.file.path.display().to_string()),
    })
}

/// 逐单元构造条目时复用的只读索引。
struct PendingContext<'a> {
    store: &'a crate::Store,
    units: &'a UnitIndex<'a>,
    show_episodes: &'a HashMap<domain::MediaId, HashSet<(u32, u32)>>,
    show_seasons: &'a HashMap<domain::MediaId, HashSet<u32>>,
}

/// 锁内收集的 owned DB 事实；文件 IO 全部留到锁外。
struct PendingSnapshot {
    file: domain::LibraryFile,
    /// 该条目覆盖的观看单元；`(-1, -1)` 表示整个作品。
    unit: (i32, i32),
    media: domain::Media,
    is_series: bool,
    parent_id: String,
    ticks: i64,
    play_count: i64,
    played: bool,
    unplayed_item_count: Option<usize>,
    chapters: Vec<library::ChapterMarker>,
    intro_start_ms: Option<i64>,
    intro_end_ms: Option<i64>,
    outro_start_ms: Option<i64>,
    outro_end_ms: Option<i64>,
    facts: item_metadata::ItemDbFacts,
    fields: domain::MetadataFields,
    child_counts: Option<(usize, usize)>,
    /// 非 series：DateCreated 需要 stat 这个路径；series：用 ingest 映射。
    stat_path: Option<String>,
}

impl PendingSnapshot {
    /// 锁外：NFO/stat/外部字幕 IO + 组装。
    fn into_snapshot(
        self,
        browse: &dyn crate::file_stat_read::FileStatRead,
        date_created: Option<SystemTime>,
    ) -> MediaItemSnapshot {
        let Self {
            file,
            unit,
            media,
            is_series,
            parent_id,
            ticks,
            play_count,
            played,
            unplayed_item_count,
            chapters,
            intro_start_ms,
            intro_end_ms,
            outro_start_ms,
            outro_end_ms,
            facts,
            fields,
            child_counts,
            stat_path: _,
        } = self;
        let metadata = with_date_created(
            item_metadata(
                facts,
                &fields,
                browse,
                &file,
                unit,
                &media,
                is_series,
                child_counts,
            ),
            date_created,
        );
        MediaItemSnapshot {
            file,
            unit: snapshot_unit(unit),
            media,
            ticks,
            play_count,
            played,
            unplayed_item_count,
            parent_id,
            is_series,
            chapters,
            intro_start_ms,
            intro_end_ms,
            outro_start_ms,
            outro_end_ms,
            metadata,
        }
    }
}

fn playback_ticks(
    store: &crate::Store,
    user_id: UserId,
    file: &domain::LibraryFile,
    unit: (i32, i32),
    media: &domain::Media,
) -> i64 {
    let (season, episode) = unit_columns(unit);
    store
        .playback_unit_progress(user_id, file.media_id, season, episode)
        .ok()
        .flatten()
        .or_else(|| {
            (media.kind == MediaKind::Movie)
                .then(|| {
                    store
                        .playback_progress(user_id, file.media_id)
                        .ok()
                        .flatten()
                })
                .flatten()
        })
        .unwrap_or(0)
        * 10_000
}

#[cfg(test)]
mod tests;
