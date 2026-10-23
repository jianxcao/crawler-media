use domain::{FileCoverage, LedgerId, Media, MediaId, Subscribe, UserId};
use std::collections::HashMap;

use crate::Store;

pub(crate) struct SubscriptionViewSnapshot {
    pub subscribe: Subscribe,
    pub media: Media,
    pub facts: subscribe::SubscribeFacts,
    pub pending: Vec<(i32, crate::store::PendingDownload)>,
    pub created_at: String,
    pub updated_at: String,
    pub library_poster_candidate: Option<LedgerId>,
    pub total: i64,
    pub imported: i64,
    pub target_library_status: &'static str,
    pub target_library_error: Option<&'static str>,
}

// Only explicit bindings are checked: an implicit target still uses normal routing.
fn target_library_state(
    sub: &Subscribe,
    targets: &HashMap<String, bool>,
) -> (&'static str, Option<&'static str>) {
    let Some(id) = sub.library_id else {
        return ("ready", None);
    };
    match targets.get(&id.to_string()) {
        Some(true) => ("ready", None),
        Some(false) => (
            "deleting",
            Some(
                "目标媒体库正在删除或已冻结，请重新选择媒体库。订阅仍保留原目标，不会自动切换到其他媒体库。",
            ),
        ),
        None => (
            "missing",
            Some(
                "目标媒体库已不存在，请重新选择媒体库。订阅仍保留原目标，不会自动切换到其他媒体库。",
            ),
        ),
    }
}

fn load_target_libraries(store: &Store) -> Result<HashMap<String, bool>, crate::store::StoreError> {
    let mut targets: HashMap<_, _> = store
        .list_libraries()?
        .into_iter()
        .map(|library| (library.id, true))
        .collect();
    for removal in store.pending_library_removals()? {
        if let Some(ready) = targets.get_mut(&removal.library_id) {
            *ready = false;
        }
    }
    Ok(targets)
}

pub(crate) fn load_visible_subscription_views(
    store: &Store,
    user_id: UserId,
) -> Result<Vec<SubscriptionViewSnapshot>, crate::store::StoreError> {
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);

    let subscribes = if admin {
        store.list_all_subscribes()?
    } else {
        store.list_subscribes_for_user(user_id)?
    };

    // 预批量提取所有相关 ledger 覆盖，按 media_id 分组
    let media_ids: Vec<MediaId> = subscribes.iter().map(|sub| sub.media_id).collect();
    let all_ledger = store
        .file_coverage_for_media_batch(&media_ids)
        .unwrap_or_default();
    let mut ledger_by_media: HashMap<MediaId, Vec<FileCoverage>> = HashMap::new();
    for coverage in all_ledger {
        ledger_by_media
            .entry(coverage.file.media_id)
            .or_default()
            .push(coverage);
    }

    // Two batched reads, independent of the number of subscriptions.
    let target_libraries = load_target_libraries(store)?;
    let mut snapshots = Vec::with_capacity(subscribes.len());

    for sub in subscribes {
        let Some(media) = store.get_media(sub.media_id).ok().flatten() else {
            continue;
        };

        let facts = store.load_subscribe_facts(sub.id).unwrap_or_default();
        let pending = store.load_pending(sub.id).unwrap_or_default();
        let (created_at, updated_at) = store.subscribe_times(sub.id).unwrap_or_default();

        let (total, imported) = super::views::progress(store, &sub, &media, &facts);

        let library_poster_candidate = ledger_by_media
            .get(&media.id)
            .and_then(|rows| select_preferred_library_poster(rows));

        let (target_library_status, target_library_error) =
            target_library_state(&sub, &target_libraries);
        snapshots.push(SubscriptionViewSnapshot {
            target_library_status,
            target_library_error,
            subscribe: sub,
            media,
            facts,
            pending,
            created_at,
            updated_at,
            library_poster_candidate,
            total,
            imported,
        });
    }

    Ok(snapshots)
}

pub(crate) fn load_single_subscription_view(
    store: &Store,
    sub: &Subscribe,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
) -> SubscriptionViewSnapshot {
    let pending = store.load_pending(sub.id).unwrap_or_default();
    let (created_at, updated_at) = store.subscribe_times(sub.id).unwrap_or_default();
    let (total, imported) = super::views::progress(store, sub, media, facts);

    let rows: Vec<FileCoverage> = store
        .file_coverage_for_media(media.id)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let library_poster_candidate = select_preferred_library_poster(&rows);

    let (target_library_status, target_library_error) = match load_target_libraries(store) {
        Ok(targets) => target_library_state(sub, &targets),
        Err(error) => {
            tracing::error!(subscribe_id = %sub.id, %error, "读取订阅目标媒体库状态失败");
            (
                "deleting",
                Some("暂时无法确认目标媒体库状态，请刷新后重试或重新选择媒体库。"),
            )
        }
    };

    SubscriptionViewSnapshot {
        target_library_status,
        target_library_error,
        subscribe: sub.clone(),
        media: media.clone(),
        facts: facts.clone(),
        pending,
        created_at,
        updated_at,
        library_poster_candidate,
        total,
        imported,
    }
}

/// 旧键 `(season.unwrap_or(0), episode.unwrap_or(0), path)` 是在 (file × unit) 的一行一行
/// 上取的：整片读作 `(0, 0)`（`None`），多 unit 文件每个 unit 各是一行。这里把每个
/// coverage 展开回它的 unit 再取最大，整片用 `WHOLE = (-1, -1)` 夹到 `(0, 0)`。
/// 平局仍按旧 `row.path: String` 的**逐字节**序（`PathBuf` 是分量序，不能直接用），
/// 选中后返回该文件自己的 `file_id`。
fn select_preferred_library_poster(rows: &[FileCoverage]) -> Option<LedgerId> {
    let selected = rows
        .iter()
        .flat_map(|coverage| coverage.units.iter().map(move |unit| (coverage, unit)))
        .max_by_key(|(coverage, unit)| {
            (
                unit.season.max(0) as u32,
                unit.episode.max(0) as u32,
                coverage.file.path.to_string_lossy().into_owned(),
            )
        })?;

    let poster = selected
        .0
        .file
        .path
        .parent()
        .map(|d| d.join("poster.jpg"))?;
    if poster.is_file() {
        Some(selected.0.file.file_id)
    } else {
        None
    }
}
