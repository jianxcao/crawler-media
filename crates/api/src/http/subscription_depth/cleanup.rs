use std::collections::HashSet;

use super::*;
use crate::http::subscriptions::deletion::{
    coverage_owned_by_subscribe_matching, other_media_subscribes, persisted_pending,
};
use crate::http::subscriptions::{ensure_subscribe_owner, resolve_subscribe_on};

pub(crate) async fn season_cleanup(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<SeasonCleanupInput>,
) -> Response {
    let sid = match precheck_cleanup(&state, &id, user_id, &body).await {
        Ok(sid) => sid,
        Err(response) => return response,
    };
    if body.seasons.is_empty() {
        return ok(json!({"queued":true, "binned":0, "pending_cleared":0})).into_response();
    }
    if !body.delete_library_files && !body.delete_torrents {
        return ok(json!({
            "queued": true,
            "binned": 0,
            "file_errors": [],
            "pending_cleared": 0,
            "removed_from_client": 0,
        }))
        .into_response();
    }
    let _reservation = match acquire_cleanup_fence(&state, sid, user_id).await {
        Ok(res) => res,
        Err(response) => return response,
    };
    let snapshot = match capture_cleanup_snapshot(&state, &id, user_id, &body).await {
        Ok(snap) => snap,
        Err(response) => return response,
    };
    let (rows, kept_seasons) = match filter_deletable_coverages(
        &state,
        &snapshot.subscribe,
        &snapshot.media,
        &snapshot.others,
        snapshot.coverages,
        &body.seasons,
    )
    .await
    {
        Ok(result) => result,
        Err(response) => return response,
    };
    let binned = if body.delete_library_files {
        match cleanup_library_files(&state, sid, rows, &body.seasons, &kept_seasons).await {
            Ok(binned) => binned,
            Err(response) => return response,
        }
    } else {
        0
    };
    let (removed_len, pending_cleared) = if body.delete_torrents {
        match cleanup_torrents(&state, sid, snapshot.pending, &body.seasons).await {
            Ok(result) => result,
            Err(response) => return response,
        }
    } else {
        (0, 0)
    };
    ok(json!({
        "queued": true,
        "binned": binned,
        "file_errors": [],
        "pending_cleared": pending_cleared,
        "removed_from_client": removed_len,
    }))
    .into_response()
}

struct CleanupSnapshot {
    subscribe: Subscribe,
    media: domain::Media,
    others: Vec<Subscribe>,
    coverages: Vec<domain::FileCoverage>,
    pending: Vec<(i32, crate::store::PendingDownload)>,
}

async fn precheck_cleanup(
    state: &ApiState,
    id: &str,
    user_id: domain::UserId,
    body: &SeasonCleanupInput,
) -> Result<domain::SubscribeId, Response> {
    let raw_id = id.to_string();
    let requires_admin = body.delete_library_files || body.delete_torrents;
    let subscribe_id = crate::http::store_wait::store_read(
        state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        move |store| {
            let (subscribe, _) = resolve_subscribe_on(store, &raw_id, user_id)?;
            if requires_admin && !user_is_admin(store, user_id) {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    "subscription.cleanup_forbidden",
                    "只有管理员可以联动清理下载任务或媒体库文件",
                ));
            }
            Ok(subscribe.id)
        },
    )
    .await
    .map_err(|error| error.response())??;
    if state.subscribe_is_deleting(subscribe_id) {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除",
        ));
    }
    Ok(subscribe_id)
}

async fn acquire_cleanup_fence(
    state: &ApiState,
    id: domain::SubscribeId,
    user_id: domain::UserId,
) -> Result<crate::management::SubscribeDeletionReservation, Response> {
    loop {
        let finished = state.subscribe_round_finished.notified();
        tokio::pin!(finished);
        finished.as_mut().enable();
        let worker = state.clone();
        match state
            .http_work
            .run_to_completion(move || begin_cleanup_fence(&worker, id, user_id))
            .await
        {
            Ok(Ok(Some(reservation))) => return Ok(reservation),
            Ok(Ok(None)) => finished.await,
            Ok(Err(response)) => return Err(response),
            Err(crate::blocking_budget::BlockingError::Busy) => {
                return Err(crate::http::store_wait::busy_response());
            }
            Err(error) => {
                tracing::error!(subscribe_id = %id, %error, "开始季清理失败");
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "subscription.cleanup_failed",
                    &error.to_string(),
                ));
            }
        }
    }
}

fn begin_cleanup_fence(
    state: &ApiState,
    id: domain::SubscribeId,
    user_id: domain::UserId,
) -> Result<Option<crate::management::SubscribeDeletionReservation>, Response> {
    state.require_writable()?;
    state.require_write_admitted()?;
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(id) {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除",
        ));
    }
    let store = state.store.as_ref();
    let Some(subscribe) = store.get_subscribe(id).map_err(store_error)? else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    };
    ensure_subscribe_owner(store, &subscribe, user_id)?;
    if !user_is_admin(store, user_id) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "subscription.cleanup_forbidden",
            "只有管理员可以联动清理下载任务或媒体库文件",
        ));
    }
    if state.active_transfers.lock().contains(&id) {
        return Ok(None);
    }
    state.mark_subscribe_deleting(id);
    drop(_guard);
    Ok(Some(crate::management::SubscribeDeletionReservation::new(
        state.clone(),
        id,
    )))
}

async fn capture_cleanup_snapshot(
    state: &ApiState,
    id: &str,
    user_id: domain::UserId,
    body: &SeasonCleanupInput,
) -> Result<CleanupSnapshot, Response> {
    let raw_id = id.to_string();
    let delete_files = body.delete_library_files;
    let delete_torrents = body.delete_torrents;
    crate::http::store_wait::store_read(
        state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        move |store| {
            let (subscribe, media) = resolve_subscribe_on(store, &raw_id, user_id)?;
            let others = if delete_files {
                other_media_subscribes(store, &subscribe)?
            } else {
                Vec::new()
            };
            let coverages = if delete_files {
                store
                    .file_coverage_for_media(subscribe.media_id)
                    .map_err(store_error)?
            } else {
                Vec::new()
            };
            let pending = if delete_torrents {
                persisted_pending(store, subscribe.id).map_err(store_error)?
            } else {
                Vec::new()
            };
            Ok(CleanupSnapshot {
                subscribe,
                media,
                others,
                coverages,
                pending,
            })
        },
    )
    .await
    .map_err(|error| error.response())?
}

async fn filter_deletable_coverages(
    state: &ApiState,
    subscribe: &Subscribe,
    media: &domain::Media,
    others: &[Subscribe],
    coverages: Vec<domain::FileCoverage>,
    seasons: &[u32],
) -> Result<(Vec<domain::FileCoverage>, HashSet<u32>), Response> {
    if coverages.is_empty() {
        return Ok((Vec::new(), HashSet::new()));
    }
    let state_clone = state.clone();
    let subscribe = subscribe.clone();
    let media = media.clone();
    let others = others.to_vec();
    let seasons: HashSet<u32> = seasons.iter().copied().collect();
    state
        .http_work
        .run_to_completion(move || {
            let store = state_clone.store.as_ref();
            let mut rows = Vec::new();
            let mut kept_seasons = HashSet::new();
            for coverage in coverages {
                let matches_selected = coverage
                    .units
                    .iter()
                    .any(|unit| !unit.is_whole() && seasons.contains(&(unit.season as u32)));
                if !matches_selected {
                    continue;
                }
                if coverage_owned_by_subscribe_matching(
                    store,
                    &subscribe,
                    &media,
                    &coverage,
                    &others,
                    |unit| !unit.is_whole() && seasons.contains(&(unit.season as u32)),
                ) {
                    rows.push(coverage);
                } else {
                    for unit in &coverage.units {
                        if !unit.is_whole() && seasons.contains(&(unit.season as u32)) {
                            kept_seasons.insert(unit.season as u32);
                        }
                    }
                }
            }
            (rows, kept_seasons)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "季清理文件归属判定工作失败");
            crate::http::store_wait::busy_response()
        })
}

async fn cleanup_library_files(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    rows: Vec<domain::FileCoverage>,
    seasons: &[u32],
    kept_seasons: &HashSet<u32>,
) -> Result<usize, Response> {
    let report =
        crate::http::file_delete::delete_coverages(state, rows, "subscription_deleted").await?;
    let clean_seasons: Vec<u32> = if report.errors.is_empty() {
        seasons
            .iter()
            .copied()
            .filter(|season| !kept_seasons.contains(season))
            .collect()
    } else {
        Vec::new()
    };
    let paths = report.paths;
    if !paths.is_empty() || !clean_seasons.is_empty() {
        crate::http::store_wait::store_write(state, move |store| {
            store
                .delete_subscribe_facts_for_paths(subscribe_id, &paths)
                .map_err(store_error)?;
            store
                .delete_subscribe_facts_for_seasons(subscribe_id, &clean_seasons)
                .map_err(store_error)?;
            Ok::<_, Response>(())
        })
        .await??;
    }
    if !report.errors.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.file_cleanup_failed",
            &report.errors.join("；"),
        ));
    }
    Ok(report.removed)
}

async fn cleanup_torrents(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    seasons: &[u32],
) -> Result<(usize, usize), Response> {
    let selected: Vec<_> = pending
        .into_iter()
        .filter_map(|(_, pending)| {
            let season = pending
                .release_override
                .as_ref()
                .and_then(|release| release.season)
                .or_else(|| release::parse(&pending.torrent.title).season);
            season
                .is_some_and(|season| seasons.contains(&season))
                .then_some(crate::http::subscriptions::TorrentRemovalTarget {
                    subscribe_id,
                    torrent: pending.torrent,
                    downloader_id: pending.downloader_id,
                })
        })
        .collect();
    let (removed, pending_cleared, torrent_errors) =
        remove_torrents_from_client(state, selected).await;
    if !torrent_errors.is_empty() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            "subscription.downloader_cleanup_failed",
            &torrent_errors.join("；"),
        ));
    }
    Ok((removed.len(), pending_cleared))
}

fn store_error(error: crate::store::StoreError) -> Response {
    tracing::error!(%error, "Subscribe cleanup preparation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
