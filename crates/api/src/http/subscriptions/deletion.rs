use std::collections::HashMap;
use std::str::FromStr;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Subscribe, SubscribeId, UserId};
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

use super::{ensure_subscribe_owner, remove_torrents_from_client, user_is_admin};

pub(crate) async fn delete_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let sid = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let snapshot = match start_subscription_deletion(
        &state,
        sid,
        user_id,
        flag(&query, "delete_torrents"),
        flag(&query, "delete_library_files"),
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    let SubscriptionDeleteSnapshot {
        subscribe,
        pending,
        rows,
        delete_torrents,
        _reservation,
    } = snapshot;
    let (removed_torrents, binned) =
        match cleanup_subscription(&state, sid, pending, rows, delete_torrents).await {
            Ok(result) => result,
            Err(response) => return response,
        };
    if let Err(response) = finalize_subscription_deletion(&state, sid, subscribe.user_id).await {
        return response;
    }
    drop(_reservation);
    ok(json!({
        "deleted": true,
        "removed_from_client": removed_torrents.len(),
        "binned": binned,
        "file_errors": [],
    }))
    .into_response()
}

fn parse_id(raw: &str) -> Result<SubscribeId, Response> {
    SubscribeId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "订阅 id 无效",
        )
    })
}

fn flag(query: &HashMap<String, String>, name: &str) -> bool {
    query.get(name).is_some_and(|value| value == "true")
}

struct SubscriptionDeleteSnapshot {
    subscribe: Subscribe,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    rows: Vec<domain::LedgerRow>,
    delete_torrents: bool,
    _reservation: crate::management::SubscribeDeletionReservation,
}

async fn start_subscription_deletion(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
    delete_torrents: bool,
    delete_library_files: bool,
) -> Result<SubscriptionDeleteSnapshot, Response> {
    let state = state.clone();
    match tokio::task::spawn_blocking(move || {
        begin_subscription_deletion(&state, id, user_id, delete_torrents, delete_library_files)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(subscribe_id = %id, %error, "开始删除订阅失败");
            Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "subscription.delete_failed",
                &error.to_string(),
            ))
        }
    }
}

async fn cleanup_subscription(
    state: &ApiState,
    subscribe_id: SubscribeId,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    rows: Vec<domain::LedgerRow>,
    delete_torrents: bool,
) -> Result<(Vec<domain::Torrent>, usize), Response> {
    let torrents = if delete_torrents {
        torrents_to_delete(subscribe_id, pending)
    } else {
        Vec::new()
    };
    let (removed, _pending_cleared, errors) = remove_torrents_from_client(state, torrents).await;
    if !errors.is_empty() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            "subscription.downloader_cleanup_failed",
            &errors.join("；"),
        ));
    }
    let (binned, errors) = crate::http::file_delete::remove_rows(state, rows).await;
    if !errors.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.file_cleanup_failed",
            &errors.join("；"),
        ));
    }
    Ok((removed, binned))
}

#[derive(Clone, Debug)]
pub(crate) struct TorrentRemovalTarget {
    pub(crate) subscribe_id: SubscribeId,
    pub(crate) torrent: domain::Torrent,
    pub(crate) downloader_id: Option<domain::DownloaderId>,
}

fn torrents_to_delete(
    subscribe_id: SubscribeId,
    pending: Vec<(i32, crate::store::PendingDownload)>,
) -> Vec<TorrentRemovalTarget> {
    pending
        .into_iter()
        .map(|(_, pending)| TorrentRemovalTarget {
            subscribe_id,
            torrent: pending.torrent,
            downloader_id: pending.downloader_id,
        })
        .collect()
}

fn persisted_pending(
    store: &crate::Store,
    id: SubscribeId,
) -> Result<Vec<(i32, crate::store::PendingDownload)>, crate::store::StoreError> {
    let mut pending = store.load_pending(id)?;
    pending.extend(store.load_pending_state(id, "imported")?);
    Ok(pending)
}

fn exclusively_owned_pending(
    store: &crate::Store,
    id: SubscribeId,
) -> Result<Vec<(i32, crate::store::PendingDownload)>, Response> {
    let result = persisted_pending(store, id);
    result.map_err(|error| {
        tracing::error!(subscribe_id = %id, %error, "读取下载任务归属失败，拒绝清理");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )
    })
}

async fn finalize_subscription_deletion(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<(), Response> {
    let state_for_delete = state.clone();
    match tokio::task::spawn_blocking(move || {
        finish_subscription_deletion(&state_for_delete, id, user_id)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(response)) => return Err(response),
        Err(error) => {
            tracing::error!(subscribe_id = %id, %error, "完成删除订阅失败");
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "subscription.delete_failed",
                &error.to_string(),
            ));
        }
    }
    state
        .jobs
        .lock()
        .delete_defs_for_payload(&format!("{{\"subscribe_id\":\"{id}\"}}"))
        .map(|_| ())
        .map_err(|error| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "subscription.job_cleanup_failed",
                &error.to_string(),
            )
        })
}

fn begin_subscription_deletion(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
    delete_torrents: bool,
    delete_library_files: bool,
) -> Result<SubscriptionDeleteSnapshot, Response> {
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(id) {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除",
        ));
    }
    let store = state.store.lock();
    let Some(subscribe) = store.get_subscribe(id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    };
    validate_delete_permission(
        &store,
        &subscribe,
        user_id,
        delete_torrents || delete_library_files,
    )?;
    let pending = if delete_torrents {
        exclusively_owned_pending(&store, id)?
    } else {
        Vec::new()
    };
    let rows = if delete_library_files {
        deletable_library_rows(&store, &subscribe)?
    } else {
        Vec::new()
    };
    state.mark_subscribe_deleting(id);
    drop(store);
    drop(_guard);
    let reservation = crate::management::SubscribeDeletionReservation::new(state.clone(), id);
    Ok(SubscriptionDeleteSnapshot {
        subscribe,
        pending,
        rows,
        delete_torrents,
        _reservation: reservation,
    })
}

fn deletable_library_rows(
    store: &crate::Store,
    subscribe: &Subscribe,
) -> Result<Vec<domain::LedgerRow>, Response> {
    let media = store
        .get_media(subscribe.media_id)
        .ok()
        .flatten()
        .ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                "media.missing",
                "订阅对应的媒体不存在",
            )
        })?;
    let others = store
        .list_all_subscribes()
        .map_err(|error| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            )
        })?
        .into_iter()
        .filter(|other| other.id != subscribe.id && other.media_id == subscribe.media_id)
        .collect::<Vec<_>>();
    Ok(store
        .ledger_for_media(subscribe.media_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row_owned_by_subscribe(store, subscribe, &media, row, &others))
        .collect())
}

fn row_owned_by_subscribe(
    store: &crate::Store,
    subscribe: &Subscribe,
    media: &domain::Media,
    row: &domain::LedgerRow,
    others: &[Subscribe],
) -> bool {
    if !coverage_owns_row(&subscribe.coverage, row) {
        return false;
    }
    if others
        .iter()
        .any(|other| coverage_owns_row(&other.coverage, row))
    {
        return false;
    }
    // 物理删除不能走「不在任何根下就回退默认库」的展示归属。库外路径
    // （手工 claim、根目录变更后的旧行）一律不删。
    let Some(owner) = crate::http::library::library_for_row_strict(store, row, media) else {
        tracing::warn!(
            path = %row.path,
            subscribe_id = %subscribe.id,
            "订阅删除跳过不属于任何媒体库根的文件"
        );
        return false;
    };
    match subscribe.library_id {
        Some(id) => owner.id == id.to_string(),
        None => owner.is_default,
    }
}

fn coverage_owns_row(coverage: &domain::Coverage, row: &domain::LedgerRow) -> bool {
    match coverage {
        domain::Coverage::Movie => row.season.is_none() && row.episode.is_none(),
        domain::Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            if row.season != Some(*season) {
                return false;
            }
            let Some(episode) = row.episode else {
                return true;
            };
            episode >= *episode_from && episode_to.map(|to| episode <= to).unwrap_or(true)
        }
    }
}

fn validate_delete_permission(
    store: &crate::Store,
    subscribe: &Subscribe,
    user_id: UserId,
    requires_admin: bool,
) -> Result<(), Response> {
    ensure_subscribe_owner(store, subscribe, user_id)?;
    if requires_admin && !user_is_admin(store, user_id) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "subscription.cleanup_forbidden",
            "只有管理员可以联动清理下载任务或媒体库文件",
        ));
    }
    Ok(())
}

fn finish_subscription_deletion(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<(), Response> {
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    let store = state.store.lock();
    let current = store.get_subscribe(id).ok().flatten();
    if current.as_ref().map(|row| row.user_id) != Some(user_id) {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    }
    match store.delete_subscribe(id) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        )),
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}
