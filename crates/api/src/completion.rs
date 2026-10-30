//! M4a: `completed` tracking state for bounded, non-wash subscriptions.
//! A completed subscription keeps its search JobDef, but the search worker
//! re-validates the library first and skips the indexers while it stays
//! complete; a lost file or widened range makes it `active` again.

use domain::{Coverage, Subscribe};

use crate::ApiState;

pub(crate) const COMPLETED: &str = "completed";

/// Every unit of a bounded, non-wash coverage owns a file that still exists.
pub(crate) fn library_complete(state: &ApiState, subscribe: &Subscribe) -> bool {
    if subscribe.wash_cut
        || matches!(
            subscribe.coverage,
            Coverage::Tv {
                episode_to: None,
                ..
            }
        )
    {
        return false;
    }
    units_on_disk(state, subscribe)
}

/// Every unit of the (bounded) coverage has a fact whose file exists.
/// Unlike `library_complete`, wash-cut subscriptions are not excluded.
pub(crate) fn units_on_disk(state: &ApiState, subscribe: &Subscribe) -> bool {
    // G-L1: only facts inside the subscription's target library count.
    let kind = match subscribe.coverage {
        Coverage::Movie => domain::MediaKind::Movie,
        Coverage::Tv { .. } => domain::MediaKind::Tv,
    };
    let Ok(facts) =
        crate::subscribe_library_read::read_subscribe_library_facts(state, subscribe, kind)
    else {
        return false;
    };
    let units = subscribe.coverage.units();
    !units.is_empty()
        && units.iter().all(|(season, episode)| {
            facts
                .get(*season, *episode)
                .and_then(|fact| fact.path.as_deref())
                .is_some_and(|path| std::path::Path::new(path).exists())
        })
}

/// Caller holds this Subscribe's guard; no observation or notification runs here.
pub(crate) fn current_is_unchanged(
    state: &ApiState,
    expected: &Subscribe,
) -> Result<bool, store::StoreError> {
    Ok(expected.tracking_state != "paused"
        && !state.subscribe_is_deleting(expected.id)
        && state.store.get_subscribe(expected.id)?.as_ref() == Some(expected))
}

/// After Transfer / search, publish completion only for the Subscribe observed.
/// The returned value includes this call's own accepted state transition.
pub(crate) fn mark_if_complete(state: &ApiState, subscribe: &Subscribe) -> Option<Subscribe> {
    if subscribe.tracking_state == "paused" {
        return None;
    }
    let complete = library_complete(state, subscribe);
    let accepted = {
        let guard = state.subscribe_guard(subscribe.id);
        let _guard = guard.lock();
        match accept_completion(state, subscribe, complete) {
            Ok(accepted) => accepted?,
            Err(error) => {
                tracing::warn!(subscribe_id = %subscribe.id, %error, "更新订阅追踪状态失败");
                return None;
            }
        }
    };
    if complete {
        create_next_season(state, &accepted);
    }
    Some(accepted)
}

fn accept_completion(
    state: &ApiState,
    subscribe: &Subscribe,
    complete: bool,
) -> Result<Option<Subscribe>, store::StoreError> {
    if !current_is_unchanged(state, subscribe)? {
        return Ok(None);
    }
    let mut accepted = subscribe.clone();
    if complete && accepted.tracking_state == "active" {
        accepted.tracking_state = COMPLETED.into();
        state.store.update_subscribe(&accepted)?;
        tracing::info!(subscribe_id = %subscribe.id, "订阅已全部入库，标记 completed，后续搜索跳过索引站");
    }
    Ok(Some(accepted))
}

fn auto_next_key(id: domain::SubscribeId) -> String {
    format!("subscribe.auto_next_season:{id}")
}

fn next_created_key(id: domain::SubscribeId) -> String {
    format!("subscribe.next_season_created:{id}")
}

/// G-M3: opt-in per subscription (stored as a setting, default off).
pub(crate) fn set_auto_next_season(state: &ApiState, id: domain::SubscribeId, on: bool) {
    let store = state.store.as_ref();
    let result = if on {
        store.put_setting(&auto_next_key(id), "1")
    } else {
        store.delete_setting(&auto_next_key(id)).map(|_| ())
    };
    if let Err(error) = result {
        tracing::warn!(subscribe_id = %id, %error, "保存 auto_next_season 失败");
    }
}

/// G-M3: once a single-season subscription completes and the catalog lists
/// season S+1, create one open subscription for it (copying filter, library,
/// downloader and wash settings). Idempotent. Call without the store lock.
fn create_next_season(state: &ApiState, subscribe: &Subscribe) {
    let Coverage::Tv { season, .. } = subscribe.coverage else {
        return;
    };
    let store = state.store.as_ref();
    let media = match next_season_enabled(store, subscribe.id).and_then(|enabled| {
        if enabled {
            store.get_media(subscribe.media_id)
        } else {
            Ok(None)
        }
    }) {
        Ok(Some(media)) => media,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "读取下一季自动订阅配置失败");
            return;
        }
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let Some(next) = season.checked_add(1) else {
        tracing::warn!(subscribe_id = %subscribe.id, season, "无法创建超出季编号范围的订阅");
        return;
    };
    match state.catalog.season_episodes(tmdb_id, next) {
        Ok(episodes) if !episodes.is_empty() => {}
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, season = next, %error, "读取下一季目录失败");
            return;
        }
    }
    let published = {
        let guard = state.subscribe_guard(subscribe.id);
        let _guard = guard.lock();
        publish_next_season(state, subscribe, &media, next)
    };
    match published {
        Ok(Some(created)) => {
            tracing::info!(subscribe_id = %subscribe.id, next_subscribe = %created, season = next, "已自动创建下一季订阅");
            crate::worker::notify(
                state,
                "已自动订阅下一季",
                &format!("「{}」第 {next} 季已创建订阅", media.title),
            );
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "自动创建下一季订阅失败");
        }
    }
}

fn next_season_enabled(
    store: &store::Store,
    id: domain::SubscribeId,
) -> Result<bool, store::StoreError> {
    Ok(store.get_setting(&auto_next_key(id))?.is_some()
        && store.get_setting(&next_created_key(id))?.is_none())
}

/// Only database work runs under the original Subscribe's decision guard.
fn publish_next_season(
    state: &ApiState,
    subscribe: &Subscribe,
    media: &domain::Media,
    next: u32,
) -> Result<Option<domain::SubscribeId>, crate::subscribe_create::CreateError> {
    let store = state.store.as_ref();
    if !current_is_unchanged(state, subscribe)?
        || !next_season_enabled(store, subscribe.id)?
        || store.get_media(subscribe.media_id)?.as_ref() != Some(media)
    {
        return Ok(None);
    }
    let mut follow = subscribe.clone();
    follow.id = domain::SubscribeId::new();
    follow.coverage = Coverage::Tv {
        season: next,
        episode_from: 1,
        episode_to: None,
    };
    follow.tracking_state = "active".into();
    match crate::subscribe_create::create(state, media.clone(), follow, None) {
        Ok((_, created)) => {
            store.put_setting(&next_created_key(subscribe.id), &created.id.to_string())?;
            store.put_setting(&auto_next_key(created.id), "1")?;
            Ok(Some(created.id))
        }
        Err(crate::subscribe_create::CreateError::Overlap(other)) => {
            store.put_setting(&next_created_key(subscribe.id), &other.to_string())?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Search worker gate. Returns true when the search should be skipped.
pub(crate) fn skip_search(state: &ApiState, subscribe: &mut Subscribe) -> bool {
    if subscribe.tracking_state != COMPLETED {
        return false;
    }
    if library_complete(state, subscribe) {
        return true;
    }
    let guard = state.subscribe_guard(subscribe.id);
    let _guard = guard.lock();
    match current_is_unchanged(state, subscribe) {
        Ok(true) => {}
        Ok(false) => return true,
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "检查订阅恢复条件失败");
            return true;
        }
    }
    let mut active = subscribe.clone();
    active.tracking_state = "active".into();
    if let Err(error) = state.store.update_subscribe(&active) {
        tracing::warn!(subscribe_id = %subscribe.id, %error, "恢复订阅追踪状态失败");
        return true;
    }
    // L4: the completion notice may fire again after the next completion.
    if let Err(error) = state
        .store
        .delete_setting(&format!("subscribe.completed:{}", subscribe.id))
    {
        tracing::warn!(subscribe_id = %subscribe.id, %error, "清除订阅完成通知标记失败");
    }
    tracing::info!(subscribe_id = %subscribe.id, "已完成订阅缺失文件或范围变化，恢复 active 并重新搜索");
    *subscribe = active;
    false
}
