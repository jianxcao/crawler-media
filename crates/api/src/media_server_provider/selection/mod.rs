//! Jellyfin「先选择后重元数据」：筛选/去重/排序/计数/取页在 store 一次短读事务里
//! 完成（`selection::page`），只为选中的那一页构建 tracks/chapters/NFO 细节。
//!
//! - 作用域先收敛（`read_scope`）：库/剧/分集/folder 只读相关的行，不再为每个
//!   series 重读整库；
//! - 选页在 store 侧用 `ProtocolItemQuery` 逐字段适配后的 SQL 完成，页内条目才
//!   回落到本地轻候选，按协议 id 对齐（物理版本由 `preferred_source` 决定）；
//! - person 过滤仍走候选级路径：它需要 catalog 参与成员判定，且 catalog 失败必须
//!   当成硬失败，不能因为「这个人没在库里」缩成更小的一页；
//! - 任何读取失败都向上抛，绝不退化成「更小的一页」。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use domain::{MediaId, MediaKind, UserId};
use media_server::provider::{MediaItemSnapshot, QueryError, ServerItemPage, ServerItemQuery};

use super::item_metadata;
use super::items::{snapshot_unit_coords, unit_columns, with_date_created};
use super::*;

mod light;
mod page;
mod request;
mod response;

pub(super) use response::{query_latest_items, query_visible_items};
mod resume;
mod scope;

use light::{LightItem, build_light_items, paths_date_created};
use response::AttemptError;
pub(super) use resume::resumable_items;
use scope::read_scope;

/// 选择阶段允许的 blocking 预算（与 `list_visible_items` 一致）。
const SELECTION_DEADLINE: Duration = Duration::from_secs(30);

async fn finish_protocol(
    state: &ApiState,
    deadline: std::time::Instant,
    user_id: UserId,
    query: ServerItemQuery,
    result: store::LibraryReadResult,
) -> Result<(ServerItemPage, Vec<domain::ProtocolItemRef>), AttemptError> {
    let store::LibraryReadResult::Protocol {
        page,
        inputs,
        series,
    } = result
    else {
        return Err(QueryError::Internal("wrong Library read result".into()).into());
    };
    count_candidates(state, page.items.len());
    state
        .jellyfin_hydrated_rows
        .fetch_add(inputs.hydrated_rows() as usize, Ordering::SeqCst);
    let references = page.items.clone();
    let cold_state = state.clone();
    let selected = crate::library_read_work::run_cold_query_stage(state, deadline, move |budget| {
        if let Some(barrier) = &cold_state.protocol_selection_barrier {
            barrier(&page);
        }
        hydrate_page(&cold_state, user_id, query, (page, inputs, series), &budget)
    })
    .await?;
    Ok((selected, references))
}

pub(super) async fn next_up_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    let series_id = series_id.map(str::to_owned);
    budget
        .run(SELECTION_DEADLINE, move || {
            next_up_blocking(&state, user_id, series_id.as_deref(), offset, limit)
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin NextUp blocking 工作失败");
            error.to_string()
        })?
}

type SelectedFacts = (
    domain::ProtocolItemPage,
    store::library_query::page_inputs::ProtocolPageInputs,
    HashMap<MediaId, store::library_query::series_scalar::ProtocolSeriesScalar>,
);

fn hydrate_page(
    state: &ApiState,
    user_id: UserId,
    query: ServerItemQuery,
    (page, mut inputs, series_facts): SelectedFacts,
    budget: &store::QueryBudget,
) -> Result<ServerItemPage, QueryError> {
    let units_started = std::time::Instant::now();
    // Multiple selected episodes may name the same pack. Move its coverage once,
    // then share it across the whole page as well as the NextUp/Resume candidates.
    let coverages = std::mem::take(&mut inputs.coverage)
        .into_iter()
        .map(|coverage| (coverage.file.file_id, Arc::new(coverage)))
        .collect();
    let units = super::items::unit_index(&inputs.user_items);
    state.record_query_phase("selected_facts", units_started.elapsed());
    let total = usize::try_from(page.total).unwrap_or(usize::MAX);
    let expected = query.limit.min(total.saturating_sub(query.offset));
    if page.items.len() != expected {
        return Err(QueryError::Internal(format!(
            "protocol selection incomplete: expected {expected}, got {}",
            page.items.len()
        )));
    }
    let mut items = Vec::with_capacity(page.items.len());
    for reference in page.items {
        let light_started = std::time::Instant::now();
        let item = light::selected_light_item(&reference, &inputs, &coverages, &units,
            series_facts.get(&reference.item.media_id()).copied())
            .map_err(|error| {
                tracing::error!(%error, item = %reference.item.to_wire_string(), "Jellyfin selected item could not be hydrated");
                QueryError::Internal(error)
            })?;
        state.record_query_phase("selected_facts", light_started.elapsed());
        let details_started = std::time::Instant::now();
        items.push(
            heavy_snapshot(state, user_id, item, Some(budget)).map_err(ColdError::into_query)?,
        );
        state.record_query_phase("cold_details", details_started.elapsed());
        count_heavy(state, 1);
    }
    if items.len() != expected {
        return Err(QueryError::Internal(format!(
            "protocol page incomplete: expected {expected}, got {}",
            items.len()
        )));
    }
    Ok(ServerItemPage { items, total })
}

fn query_store_error(error: store::StoreError) -> QueryError {
    if crate::library_read_work::store_error_is_unavailable(&error) {
        QueryError::Unavailable
    } else {
        QueryError::Internal(error.to_string())
    }
}

fn next_up_blocking(
    state: &ApiState,
    user_id: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let query = next_up_query(series_id);
    let scope = {
        let store = state.store.lock();
        let scope = match read_scope(&store, user_id, &query) {
            Ok(scope) => scope,
            Err(error) => {
                count_candidates(state, error.rows_read);
                return Err(error.to_string());
            }
        };
        scope
    };
    count_candidates(state, scope.rows_read);
    if scope.unresolved {
        return Ok(ServerItemPage::default());
    }
    // The scope owns the captured coverage and Playback facts. Candidate
    // expansion/grouping/sorting is pure work and never holds the writer mutex.
    let items = build_light_items(&scope, &query);
    // 一次聚合：候选按 Media 分组（保持候选顺序），每个 Media 取 (季, 集) 最小的
    // 未看条目，再整体分页。显式给了 SeriesId 时按旧语义允许任意 Media（含电影）。
    let restrict_kind = series_id.is_none();
    let mut order: Vec<MediaId> = Vec::new();
    let mut groups: HashMap<MediaId, Vec<LightItem>> = HashMap::new();
    for item in items {
        if item.snapshot.is_series || (restrict_kind && item.snapshot.media.kind != MediaKind::Tv) {
            continue;
        }
        let media_id = item.snapshot.media.id;
        let group = groups.entry(media_id).or_default();
        if group.is_empty() {
            order.push(media_id);
        }
        group.push(item);
    }
    let mut next_up = Vec::new();
    for media_id in order {
        let mut group = groups.remove(&media_id).unwrap_or_default();
        group.sort_by_key(|item| {
            // 整片没有坐标，排序键退化成 0（与旧 `Option<u32>` 的
            // `unwrap_or_default()` 同值），不能写成 `UNIT_WHOLE`（-1）。
            (
                item.snapshot
                    .unit
                    .map(|unit| unit.season)
                    .unwrap_or_default(),
                item.snapshot
                    .unit
                    .map(|unit| unit.episode)
                    .unwrap_or_default(),
            )
        });
        if let Some(episode) = group.into_iter().find(|item| !item.snapshot.played) {
            next_up.push(episode);
        }
    }
    let total = next_up.len();
    let selected: Vec<LightItem> = next_up.into_iter().skip(offset).take(limit).collect();
    let mut page_items = Vec::with_capacity(selected.len());
    for item in selected {
        page_items
            .push(heavy_snapshot(state, user_id, item, None).map_err(|error| error.to_string())?);
        count_heavy(state, 1);
    }
    Ok(ServerItemPage {
        items: page_items,
        total,
    })
}

/// NextUp 的候选读只需要「这个用户能看到的行」或「这个 Media 的行」。
///
/// 这里没有 `limit`：NextUp 的取页发生在「每个 Media 第一条未看」这个分组之后，
/// 无法用一条 `LIMIT` 表达（`protocol_item_page` 不支持按播放状态排序/分组），
/// 所以候选读必须覆盖作用域内的全部分集行；**水合仍然只有返回页那几条**。
fn next_up_query(series_id: Option<&str>) -> ServerItemQuery {
    ServerItemQuery {
        series_id: series_id.map(str::to_owned),
        // NextUp 要的是分集候选：显式只列分集，避免为「剧折叠」条目建快照。
        include_item_types: Some("Episode".into()),
        recursive: true,
        ..Default::default()
    }
}

#[derive(Debug, thiserror::Error)]
enum ColdError {
    #[error("query temporarily unavailable")]
    Unavailable,
    #[error("{0}")]
    Internal(String),
}
impl ColdError {
    fn from_store(error: store::StoreError) -> Self {
        if crate::library_read_work::store_error_is_unavailable(&error) {
            Self::Unavailable
        } else {
            Self::Internal(error.to_string())
        }
    }
    fn from_read(error: store::LibraryReadError) -> Self {
        if crate::library_read_work::error_is_unavailable(&error) {
            Self::Unavailable
        } else {
            Self::Internal(error.to_string())
        }
    }
    fn into_query(self) -> QueryError {
        match self {
            Self::Unavailable => QueryError::Unavailable,
            Self::Internal(message) => QueryError::Internal(message),
        }
    }
}

type HeavyDbFacts = (
    item_metadata::ItemDbFacts,
    domain::MetadataFields,
    Vec<library::ChapterMarker>,
    Option<store::StoredMediaMarker>,
);

/// Collect cold DB facts with the request budget, then release Store before rendering.
fn heavy_db_facts(
    state: &ApiState,
    item: &LightItem,
    budget: Option<&store::QueryBudget>,
) -> Result<HeavyDbFacts, ColdError> {
    let file = &item.coverage.file;
    let coords = snapshot_unit_coords(item.snapshot.unit);
    let (season, episode) = unit_columns(coords);
    let is_series = item.snapshot.is_series;
    let media = &item.snapshot.media;
    let load = |store: &crate::Store| -> Result<HeavyDbFacts, ColdError> {
        let facts = item_metadata::item_db_facts(
            store,
            file,
            coords,
            is_series,
            item.unit.as_ref(),
            item.whole_unit.as_ref(),
        );
        let fields = item_metadata::page_fields(store, file, coords, media, is_series)
            .map_err(ColdError::from_store)?;
        let chapters = store
            .get_cached_chapters(&file.file_id.to_string())
            .map_err(ColdError::from_store)?
            .unwrap_or_default();
        let marker = store
            .get_media_marker(file.media_id, season, episode)
            .map_err(ColdError::from_store)?;
        Ok((facts, fields, chapters, marker))
    };
    if let Some(budget) = budget {
        budget.check().map_err(ColdError::from_read)?;
        let store = state
            .store
            .try_lock_until(budget.deadline)
            .ok_or(ColdError::Unavailable)?;
        budget.check().map_err(ColdError::from_read)?;
        store
            .with_query_budget(budget, |store| Ok(load(store)))
            .map_err(ColdError::from_read)?
    } else {
        let store = state.store.lock();
        load(&store)
    }
}

/// 本页重元数据：tracks/chapters/intro-outro/NFO/stat/外部字幕都在这里。
fn heavy_snapshot(
    state: &ApiState,
    _user_id: UserId,
    item: LightItem,
    budget: Option<&store::QueryBudget>,
) -> Result<MediaItemSnapshot, ColdError> {
    let (facts, fields, chapters, marker) = heavy_db_facts(state, &item, budget)?;
    let browse = state.file_stats.as_ref();
    let LightItem {
        snapshot,
        coverage,
        child_counts,
        stat_paths,
        catalog,
        date_created,
        unit: _,
        whole_unit: _,
    } = item;
    let MediaItemSnapshot {
        file,
        unit: snapshot_unit,
        media,
        ticks,
        play_count,
        played,
        unplayed_item_count,
        parent_id,
        is_series,
        ..
    } = snapshot;
    let coords = snapshot_unit_coords(snapshot_unit);
    let mut metadata = item_metadata::item_metadata(
        facts,
        &fields,
        browse,
        &coverage.file,
        coords,
        &media,
        is_series,
        child_counts,
    );
    if let Some(catalog) = catalog.as_ref() {
        metadata = catalog.apply(metadata, &media);
    }
    if !is_series {
        // 探测不能改变 person 成员或计数，只为已经选中的页安排，且不持有 Store 锁。
        if state.instance_writable() {
            if let Some(budget) = budget {
                match state.probe.request_probe_query(
                    &coverage,
                    budget,
                    state.instance_writable.clone(),
                ) {
                    Ok(_) => {}
                    Err(
                        store::LibraryReadError::Deadline
                        | store::LibraryReadError::Cancelled
                        | store::LibraryReadError::Busy,
                    ) => return Err(ColdError::Unavailable),
                    Err(error) => {
                        tracing::warn!(%error, file_id = %coverage.file.file_id, "Page auto-probe admission failed")
                    }
                }
            } else {
                crate::http::library::ensure_probe_enqueued(state, &coverage);
            }
        } else {
            tracing::warn!(file_id = %coverage.file.file_id, "Auto-probe skipped in read-only instance");
        }
    }
    let date_created = date_created.or_else(|| paths_date_created(browse, is_series, &stat_paths));
    if let Some(budget) = budget {
        budget.check().map_err(ColdError::from_read)?;
    }
    Ok(MediaItemSnapshot {
        file,
        unit: snapshot_unit,
        media,
        ticks,
        play_count,
        played,
        unplayed_item_count,
        parent_id,
        is_series,
        chapters,
        intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
        metadata: with_date_created(metadata, date_created),
    })
}

fn count_candidates(state: &ApiState, candidates: usize) {
    state
        .jellyfin_candidates
        .fetch_add(candidates, Ordering::SeqCst);
}

fn count_heavy(state: &ApiState, heavy: usize) {
    state
        .jellyfin_heavy_items
        .fetch_add(heavy, Ordering::SeqCst);
    tracing::debug!(
        heavy_items = heavy,
        candidates = state.jellyfin_candidates.load(Ordering::SeqCst),
        "Jellyfin 选择工作量：候选与重元数据条数"
    );
}

/// 逻辑条目去重：协议 id 相同的多份物理版本只保留最佳物理源。
///
/// 保留顺序是候选首次出现的顺序；同一逻辑条目取 `file_revision` 最大的一份，
/// revision 相同取 `file_id` 最小的一份，播放因此仍偏向最新版本。
fn dedupe_by_logical_item(items: Vec<LightItem>) -> Vec<LightItem> {
    let mut order: Vec<domain::TypedItemId> = Vec::new();
    let mut best: HashMap<domain::TypedItemId, LightItem> = HashMap::new();
    for item in items {
        let key = item.snapshot.item_identity();
        match best.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                order.push(key);
                slot.insert(item);
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                if source_preference(&item) < source_preference(slot.get()) {
                    slot.insert(item);
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|key| best.remove(&key))
        .collect()
}

/// 物理源偏好：`file_revision` 越大越好，同 revision 时 `file_id` 越小越好。
fn source_preference(item: &LightItem) -> (std::cmp::Reverse<i64>, uuid::Uuid) {
    (
        std::cmp::Reverse(item.snapshot.file.version.revision),
        item.snapshot.file.file_id.as_uuid(),
    )
}
