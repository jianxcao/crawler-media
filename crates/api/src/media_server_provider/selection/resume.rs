//! `/Users/{id}/Items/Resume` 的选页：续播候选（最近观看倒序、每个 Media 只出第一条）
//! 在 store 的一次短读里收敛成「完整条数 + 请求页」，只有返回页那几条才建重元数据。
//!
//! 候选实体来自 `user_resumable_units`（索引化的用户观看单元），作用域读只覆盖这些
//! Media 的台账行，不再整库枚举；锁内捕获 owned 事实，过滤/去重/排序/分页与
//! 页内 tracks/chapters/NFO/stat 都在锁外执行。

use std::collections::{HashMap, HashSet};

use domain::{MediaId, TypedItemId};
use media_server::provider::{ServerItemPage, ServerItemQuery};

use super::super::items::{effective_played, unit_index};
use super::super::resume_item_id;
use super::light::{LightItem, build_light_items};
use super::scope::{ScopedRead, read_media_scope};
use super::*;

/// `user_resumable_units` 的一行：`(media, season, episode, position_ms, watch_activity_at)`。
type ResumeTarget = (MediaId, i32, i32, i64, i64);

/// 同一份 owned 事实选出的续播页：`total` 是完整过滤后的续播条数，`page` 是请求窗口内的轻条目。
struct ResumeSelection {
    total: usize,
    page: Vec<LightItem>,
}

pub(in crate::media_server_provider) async fn resumable_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    budget
        .run(SELECTION_DEADLINE, move || {
            resume_page_blocking(&state, user_id, offset, limit)
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin 续播选页 blocking 工作失败");
            error.to_string()
        })?
}

/// 锁内捕获候选与目标 Media 的事实，释放锁后过滤/去重/分页并水合返回页。
fn resume_page_blocking(
    state: &ApiState,
    user_id: UserId,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let scoped = super::super::sync_operations::read_store(state, |store| {
        let targets = store.user_resumable_units(user_id).map_err(|error| {
            tracing::error!(%error, %user_id, "Jellyfin 续播候选读取失败");
            error.to_string()
        })?;
        if targets.is_empty() {
            tracing::debug!(%user_id, "Jellyfin 续播候选为空");
            return Ok(None);
        }
        let media_ids = resume_media_ids(&targets);
        let scope = match read_media_scope(store, user_id, &media_ids) {
            Ok(scope) => scope,
            Err(error) => {
                count_candidates(state, error.rows_read);
                tracing::error!(%user_id, rows_read = error.rows_read, error = %error, "Jellyfin 续播作用域读取失败");
                return Err(error.to_string());
            }
        };
        Ok(Some((targets, scope)))
    })?;
    let Some((targets, scope)) = scoped else {
        return Ok(ServerItemPage::default());
    };
    count_candidates(state, scope.rows_read);
    let selection = resume_selection(&scope, user_id, &targets, offset, limit);
    let total = selection.total;
    let hydration = hydration::load(state, &selection.page)?;
    let mut items = Vec::with_capacity(selection.page.len());
    for item in selection.page {
        let facts = hydration.get(&item.coverage.file.file_id);
        items.push(
            heavy_snapshot(state, user_id, item, facts, None, None)
                .map_err(|error| error.to_string())?,
        );
        count_heavy(state, 1);
    }
    tracing::debug!(%user_id, total, returned = items.len(), "Jellyfin 续播选页完成");
    Ok(ServerItemPage { items, total })
}

/// 目标 Media（保持候选顺序去重）：台账读取只覆盖这些媒体。
fn resume_media_ids(targets: &[ResumeTarget]) -> Vec<MediaId> {
    let mut seen = HashSet::new();
    let mut media_ids = Vec::new();
    for (media_id, _, _, _, _) in targets {
        if seen.insert(*media_id) {
            media_ids.push(*media_id);
        }
    }
    media_ids
}

/// 续播语义（与旧 `list_resumable_items` 一致）：同一 Media 只出第一条，被更晚的
/// 已看标记覆盖的候选跳过，水合后仍为已看的候选跳过。
fn resume_selection(
    scope: &ScopedRead,
    user_id: UserId,
    targets: &[ResumeTarget],
    offset: usize,
    limit: usize,
) -> ResumeSelection {
    let mut items: Vec<Option<LightItem>> =
        dedupe_by_logical_item(build_light_items(scope, &resume_query()))
            .into_iter()
            .map(Some)
            .collect();
    let mut index: HashMap<TypedItemId, usize> = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        if let Some(item) = item {
            index.insert(item.snapshot.item_identity(), position);
        }
    }
    let units = unit_index(&scope.units);
    let mut seen_media: HashSet<MediaId> = HashSet::new();
    let mut survivors: Vec<LightItem> = Vec::new();
    for (media_id, season, episode, _position_ms, _updated_at) in targets {
        if seen_media.contains(media_id) {
            continue;
        }
        if effective_played(&units, *media_id, *season, *episode) {
            tracing::debug!(%user_id, %media_id, season, episode, "跳过被更晚已看标记覆盖的续播候选");
            continue;
        }
        let Some(item) = index
            .get(&resume_item_id(*media_id, *season, *episode))
            .and_then(|position| items[*position].take())
        else {
            continue;
        };
        if item.snapshot.played {
            continue;
        }
        seen_media.insert(*media_id);
        survivors.push(item);
    }
    let total = survivors.len();
    let page = survivors.into_iter().skip(offset).take(limit).collect();
    ResumeSelection { total, page }
}

/// 续播候选的轻条目形态：作用域已收敛到 Media，一行即一个续播单元，不做剧折叠。
/// 不分页（`limit` 对轻候选没有意义）：取页由 `resume_selection` 在过滤后完成。
fn resume_query() -> ServerItemQuery {
    ServerItemQuery {
        recursive: true,
        ..Default::default()
    }
}
