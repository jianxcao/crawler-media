use crate::http::store_wait::{busy_response, store_read, with_foreground_store, with_store_read};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use domain::DownloaderId;
use serde_json::{Value, json};

use super::tasks::task_state;
use crate::http::ok;
use crate::management::ApiState;
use crate::pending_reconcile::{absence_is_provable, snapshot_matches_pending_with_hash};

fn prune_orphan_pending(
    store: &crate::Store,
) -> (
    Vec<(domain::SubscribeId, i32, store::PendingDownload)>,
    HashSet<Option<DownloaderId>>,
    Vec<(domain::SubscribeId, String, String)>,
) {
    let rows = store.list_all_pending_routed().unwrap_or_default();
    let mut kept = Vec::with_capacity(rows.len());
    let mut orphans = Vec::new();
    for (subscribe_id, score, item) in rows {
        match store.get_subscribe(subscribe_id) {
            Ok(Some(_)) => kept.push((subscribe_id, score, item)),
            Ok(None) => orphans.push((subscribe_id, item.torrent.enclosure, item.torrent.title)),
            Err(error) => {
                tracing::error!(%subscribe_id, %error, "读取订阅失败，保留 pending 行");
                kept.push((subscribe_id, score, item));
            }
        }
    }
    let mut targets: HashSet<Option<DownloaderId>> =
        kept.iter().map(|(_, _, item)| item.downloader_id).collect();
    if let Ok(owned) = store.list_owned_download_tasks() {
        targets.extend(owned.into_iter().map(|item| Some(item.downloader_id)));
    }
    (kept, targets, orphans)
}

fn cleanup_orphan_pending(state: &ApiState, orphans: Vec<(domain::SubscribeId, String, String)>) {
    if orphans.is_empty()
        || state.require_writable().is_err()
        || state.require_write_admitted().is_err()
    {
        return;
    }
    for (subscribe_id, enclosure, torrent) in orphans {
        if let Err(error) = with_foreground_store(state, |store| {
            store.delete_pending(subscribe_id, &enclosure)
        })
        .map_err(|response| format!("pending cleanup rejected ({})", response.status()))
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            tracing::warn!(%subscribe_id, %error, "清理孤儿 pending 跳过或失败");
        } else {
            tracing::info!(%subscribe_id, %torrent, "清理孤儿 pending：订阅已删除");
        }
    }
}

async fn load_task_snapshots(
    state: &ApiState,
    targets: HashSet<Option<DownloaderId>>,
) -> (
    HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    HashSet<Option<DownloaderId>>,
) {
    let snapshot_state = state.clone();
    state.http_work.run(std::time::Duration::from_secs(12), move || {
        let mut by_dl: HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>> = HashMap::new();
        let mut reachable = HashSet::new();
        for id in targets {
            if let Ok(client) = crate::delivery::client_for_id(&snapshot_state, id) {
                match client.task_snapshots() {
                    Ok(snapshots) => {
                        reachable.insert(id);
                        by_dl.insert(id, snapshots);
                    }
                    Err(error) => {
                        tracing::warn!(downloader = ?id, %error, "读取下载器任务快照失败，无法对齐任务状态");
                    }
                }
            }
        }
        (by_dl, reachable)
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "Downloader task snapshots unavailable; pending rows retained");
        Default::default()
    })
}

fn prune_absent_pending(
    state: &ApiState,
    pending: Vec<(domain::SubscribeId, i32, store::PendingDownload)>,
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
) -> Result<Vec<(domain::SubscribeId, i32, store::PendingDownload)>, Response> {
    let now = crate::job_loop::unix_now();
    let mut hashes: HashMap<domain::SubscribeId, HashMap<String, String>> = HashMap::new();
    let mut kept = Vec::with_capacity(pending.len());
    for (subscribe_id, score, item) in pending {
        let snapshots = snapshots_by_downloader
            .get(&item.downloader_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // Review M-1: same rule as the background reconcile, including the
        // persisted info hash (D-M3).
        if let std::collections::hash_map::Entry::Vacant(entry) = hashes.entry(subscribe_id) {
            let rows = with_store_read(state, &[store::Db::Subscribe], |store| {
                store
                    .load_pending_info_hashes(subscribe_id)
                    .unwrap_or_default()
            })?;
            entry.insert(rows);
        }
        let sub_hashes = &hashes[&subscribe_id];
        let hash = sub_hashes.get(&item.torrent.enclosure).map(String::as_str);
        let reachable_list = reachable.contains(&item.downloader_id).then_some(snapshots);
        let matched = snapshots.iter().any(|s| {
            crate::pending_reconcile::snapshot_matches_pending_with_hash(&item.torrent, hash, s)
        });
        if !matched && reachable_list.is_some() && !absence_is_provable(&item.torrent, snapshots) {
            tracing::error!(%subscribe_id, torrent = %item.torrent.title,
                "下载器未暴露归属标签，无法证明任务已消失，拒绝自动清理 pending");
        }
        let absent = crate::pending_reconcile::is_absent_and_stale_with_hash(
            &item,
            hash,
            reachable_list,
            now,
        );
        if absent && delete_absent_pending(state, subscribe_id, &item) {
            continue;
        }
        kept.push((subscribe_id, score, item));
    }
    Ok(kept)
}

/// Delete one absent pending row under its Subscribe guard (lock order:
/// guard → store, same as search/Transfer). A busy guard means a search or
/// Transfer is working on this Subscribe right now: skip this round.
fn delete_absent_pending(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    item: &store::PendingDownload,
) -> bool {
    if state.require_writable().is_err() || state.require_write_admitted().is_err() {
        tracing::debug!(%subscribe_id, "Protected task read skips absent pending cleanup");
        return false;
    }
    let guard = state.subscribe_guard(subscribe_id);
    let Some(_held) = guard.try_lock() else {
        tracing::debug!(%subscribe_id, "订阅正在搜索/转存，本轮跳过缺失任务清理");
        return false;
    };
    with_foreground_store(state, |store| {
        if state.require_writable().is_err() || state.require_write_admitted().is_err() {
            tracing::debug!(%subscribe_id, "Protected task read skips absent pending cleanup");
            return false;
        }
        // Re-check under the guard: Transfer may have imported it meanwhile.
        let still_active = store
            .load_pending(subscribe_id)
            .map(|rows| {
                rows.iter()
                    .any(|(_, p)| p.torrent.enclosure == item.torrent.enclosure)
            })
            .unwrap_or(false);
        if !still_active {
            return true;
        }
        match store.delete_pending(subscribe_id, &item.torrent.enclosure) {
            Ok(()) => {
                tracing::info!(%subscribe_id, torrent = %item.torrent.title, "自动清理：种子已不在下载器且超过宽限期");
                true
            }
            Err(error) => {
                tracing::error!(%subscribe_id, %error, "自动清理缺失任务失败");
                false
            }
        }
    })
    .unwrap_or_else(|response| {
        tracing::debug!(%subscribe_id, status = %response.status(), "Protected task read skips absent pending cleanup");
        false
    })
}

fn subscribe_media_index(
    store: &store::Store,
    kept: &[(domain::SubscribeId, i32, store::PendingDownload)],
) -> HashMap<String, (String, String, String)> {
    let mut subscribe_media: HashMap<String, (String, String, String)> = HashMap::new();
    for (subscribe_id, _, _) in kept {
        let key = subscribe_id.to_string();
        if subscribe_media.contains_key(&key) {
            continue;
        }
        if let Ok(Some(subscribe)) = store.get_subscribe(*subscribe_id)
            && let Ok(Some(media)) = store.get_media(subscribe.media_id)
        {
            subscribe_media.insert(
                key,
                (
                    media.title,
                    media.kind.as_str().to_string(),
                    media.id.to_string(),
                ),
            );
        }
    }
    subscribe_media
}

fn task_row_json(
    store: &store::Store,
    subscribe_id: domain::SubscribeId,
    score: i32,
    pending: &store::PendingDownload,
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
    media_info: Option<&(String, String, String)>,
    stored_hash: Option<&str>,
    is_admin: bool,
) -> Value {
    let torrent = &pending.torrent;
    let key = subscribe_id.to_string();
    let (media_title, media_kind, media_item_id) = media_info.cloned().unwrap_or_default();
    let subscribed = !media_title.is_empty();
    let snapshot = snapshots_by_downloader
        .get(&pending.downloader_id)
        .and_then(|list| {
            list.iter()
                .find(|s| snapshot_matches_pending_with_hash(torrent, stored_hash, s))
        });
    let state = match snapshot {
        Some(s) => task_state(&s.state, s.progress),
        None if reachable.contains(&pending.downloader_id) => "missing",
        None => "queued",
    };
    let row_id = if is_admin {
        format!("{}:{}", key, torrent.enclosure)
    } else {
        format!("{}:{}", key, downloader::ownership_tag(&torrent.enclosure))
    };
    json!({
        "id": row_id,
        "info_hash": snapshot.map(|s| s.info_hash.as_str()).filter(|h| !h.is_empty()).or(stored_hash),
        "name": torrent.title,
        "downloader_id": pending.downloader_id.map(|id| id.to_string()),
        "downloader_name": pending.downloader_id.and_then(|id|
            store.get_downloader(id).ok().flatten().map(|row| row.name)),
        "progress": snapshot.map(|s| s.progress),
        "size_bytes": snapshot.map(|s| s.size_bytes).or(torrent.size_bytes),
        "dlspeed_bytes": snapshot.map(|s| s.download_speed),
        "upspeed_bytes": snapshot.map(|s| s.upload_speed),
        "uploaded_bytes": snapshot.map(|s| s.uploaded_bytes),
        "completed_bytes": snapshot.map(|s| s.downloaded_bytes),
        "state": state,
        "site_id": torrent.site_id.to_string(),
        "media_item_id": if subscribed { Some(media_item_id.clone()) } else { None },
        "media_title": if subscribed { Some(media_title.clone()) } else { None },
        "media_kind": if subscribed { Some(media_kind.clone()) } else { None },
        "score": score,
        "subscriptions": if subscribed { vec![json!({
            "id": key,
            "media_item_id": media_item_id,
            "media_title": media_title,
            "media_kind": media_kind,
        })] } else { vec![] },
    })
}

fn owned_task_row_json(
    store: &store::Store,
    owned: &store::download_operations::OwnedDownloadTaskSummary,
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
) -> Value {
    let target_dl = Some(owned.downloader_id);
    let norm_hash = owned
        .info_hash
        .as_deref()
        .map(|h| h.trim().to_ascii_lowercase());
    let snapshot = snapshots_by_downloader.get(&target_dl).and_then(|list| {
        norm_hash
            .as_deref()
            .and_then(|hash| list.iter().find(|s| s.info_hash.eq_ignore_ascii_case(hash)))
            .or_else(|| {
                (!owned.ownership_tag.is_empty()).then(|| {
                    list.iter()
                        .find(|s| s.owned_tags.iter().any(|t| t == &owned.ownership_tag))
                })?
            })
    });
    let state = match snapshot {
        Some(s) => task_state(&s.state, s.progress),
        None => match owned.boost_state {
            Some(
                s @ (store::download_operations::BoostTaskState::RetainedOnly
                | store::download_operations::BoostTaskState::CleanupPlanned),
            ) => s.as_str(),
            _ if reachable.contains(&target_dl) => "missing",
            _ => "queued",
        },
    };
    json!({
        "id": owned.task_id,
        "task_id": owned.task_id,
        "info_hash": owned.info_hash.as_deref().filter(|h| !h.is_empty()).or_else(|| snapshot.map(|s| s.info_hash.as_str()).filter(|h| !h.is_empty())),
        "name": snapshot
            .map(|s| s.name.clone())
            .or_else(|| owned.info_hash.clone())
            .unwrap_or_else(|| {
                if !owned.ownership_tag.is_empty() {
                    owned.ownership_tag.clone()
                } else {
                    format!("owned-task-{}", owned.task_id)
                }
            }),
        "downloader_id": owned.downloader_id.to_string(),
        "downloader_name": store.get_downloader(owned.downloader_id).ok().flatten().map(|row| row.name),
        "progress": snapshot.map(|s| s.progress),
        "size_bytes": snapshot.map(|s| s.size_bytes).or_else(|| owned.verified_bytes.and_then(|v| u64::try_from(v).ok())),
        "dlspeed_bytes": snapshot.map(|s| s.download_speed),
        "upspeed_bytes": snapshot.map(|s| s.upload_speed),
        "uploaded_bytes": snapshot.map(|s| s.uploaded_bytes),
        "completed_bytes": snapshot.map(|s| s.downloaded_bytes),
        "state": state,
        "site_id": owned.site_id.map(|id| id.to_string()),
        "plugin_id": owned.plugin_id,
        "ownership_tag": owned.ownership_tag,
        "retained_bytes": u64::try_from(owned.retained_bytes).unwrap_or(0),
        "media_item_id": Value::Null,
        "media_title": Value::Null,
        "media_kind": Value::Null,
        "score": 0,
        "subscriptions": [],
    })
}

fn map_task_rows(
    store: &crate::Store,
    kept: &[(domain::SubscribeId, i32, store::PendingDownload)],
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
    user_id: domain::UserId,
) -> Vec<Value> {
    let is_admin = store.user_role(user_id).is_ok_and(|r| r == "admin");
    let subscribe_media = subscribe_media_index(store, kept);
    let default_dl_id = store
        .default_downloader()
        .ok()
        .flatten()
        .map(|r| r.id)
        .unwrap_or_else(|| DownloaderId::from_uuid(uuid::Uuid::nil()));
    let mut hashes_by_sub: HashMap<domain::SubscribeId, HashMap<String, String>> = HashMap::new();
    let mut rows: Vec<Value> = Vec::with_capacity(kept.len());
    let mut seen_hashes: HashSet<(DownloaderId, String)> = HashSet::new();
    for (subscribe_id, score, pending) in kept {
        let key = subscribe_id.to_string();
        let hashes = hashes_by_sub.entry(*subscribe_id).or_insert_with(|| {
            store
                .load_pending_info_hashes(*subscribe_id)
                .unwrap_or_default()
        });
        let stored_hash = hashes.get(&pending.torrent.enclosure).map(String::as_str);
        let row = task_row_json(
            store,
            *subscribe_id,
            *score,
            pending,
            snapshots_by_downloader,
            reachable,
            subscribe_media.get(&key),
            stored_hash,
            is_admin,
        );
        let effective_dl = pending.downloader_id.unwrap_or(default_dl_id);
        if let Some(hash) = row
            .get("info_hash")
            .and_then(Value::as_str)
            .filter(|h| !h.is_empty())
            .or(stored_hash)
            .filter(|h| !h.is_empty())
        {
            seen_hashes.insert((effective_dl, hash.trim().to_ascii_lowercase()));
        }
        rows.push(row);
    }
    if let Ok(owned) = store.list_owned_download_tasks() {
        for item in owned {
            let duplicate = item.info_hash.as_deref().is_some_and(|h| {
                seen_hashes.contains(&(item.downloader_id, h.trim().to_ascii_lowercase()))
            });
            if !duplicate {
                rows.push(owned_task_row_json(
                    store,
                    &item,
                    snapshots_by_downloader,
                    reachable,
                ));
            }
        }
    }
    rows
}

pub(crate) async fn list_tasks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let initial_state = state.clone();
    let (pending, targets, orphans) = match state
        .http_work
        .run_to_completion(move || {
            with_store_read(&initial_state, &[store::Db::Subscribe], |store| {
                prune_orphan_pending(store)
            })
        })
        .await
    {
        Ok(Ok(inputs)) => inputs,
        Ok(Err(response)) => return response,
        Err(crate::blocking_budget::BlockingError::Busy) => return busy_response(),
        Err(error) => {
            tracing::error!(%error, "Downloader pending capture worker failed");
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "读取失败",
            );
        }
    };
    if !orphans.is_empty() {
        let cleanup_state = state.clone();
        let _ = state
            .http_work
            .run_to_completion(move || cleanup_orphan_pending(&cleanup_state, orphans))
            .await;
    }
    let (snapshots, reachable) = load_task_snapshots(&state, targets).await;
    let snapshots = Arc::new(snapshots);
    let reachable = Arc::new(reachable);
    let worker = state.clone();
    let observed = snapshots.clone();
    let contacted = reachable.clone();
    let kept = match state
        .http_work
        .run_to_completion(move || prune_absent_pending(&worker, pending, &observed, &contacted))
        .await
    {
        Ok(Ok(kept)) => kept,
        Ok(Err(response)) => return response,
        Err(error) => {
            tracing::warn!(%error, "Downloader pending reconciliation worker unavailable");
            return busy_response();
        }
    };
    match store_read(
        &state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        move |store| map_task_rows(store, &kept, &snapshots, &reachable, user_id),
    )
    .await
    {
        Ok(items) => ok(json!({ "items": items })).into_response(),
        Err(error) => error.response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::pending_reconcile::{absence_is_provable, snapshot_matches_pending};

    fn torrent(enclosure: &str) -> domain::Torrent {
        domain::Torrent {
            site_id: domain::SiteId::new(),
            title: "The.Matrix.1999.2160p".into(),
            enclosure: enclosure.into(),
            size_bytes: None,
            seeders: None,
            free: false,
            hr: None,
            imdb_id: None,
            id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        }
    }

    fn snapshot(owned: &[String]) -> downloader::TaskSnapshot {
        downloader::TaskSnapshot {
            tag: downloader::TASK_TAG.into(),
            owned_tags: owned.to_vec(),
            info_hash: "aaaa".into(),
            ..Default::default()
        }
    }

    #[test]
    fn http_pending_matches_real_client_snapshot_by_owned_tag() {
        let t = torrent("https://pt.example/download.php?id=1");
        let owned = downloader::ownership_tag(&t.enclosure);
        assert!(snapshot_matches_pending(&t, &snapshot(&[owned])));
        let other = downloader::ownership_tag("https://pt.example/download.php?id=2");
        assert!(!snapshot_matches_pending(&t, &snapshot(&[other])));
        assert!(!snapshot_matches_pending(&t, &snapshot(&[])));
    }

    #[test]
    fn a_per_torrent_tag_spelling_never_proves_ownership() {
        let t = torrent("https://pt.example/download.php?id=1");
        let owned = downloader::ownership_tag(&t.enclosure);
        // Old clients wrote `crawler-media-<hash>` per-torrent tags and some
        // fixtures carried them in `tag`; neither spelling is our marker today.
        let per_torrent_tagged = downloader::TaskSnapshot {
            tag: "crawler-media-0f1e2d3c4b5a".into(),
            owned_tags: vec![],
            info_hash: "aaaa".into(),
            ..Default::default()
        };
        assert!(!snapshot_matches_pending(&t, &per_torrent_tagged));
        assert!(!snapshot_matches_pending(
            &t,
            &snapshot(std::slice::from_ref(&per_torrent_tagged.tag))
        ));
        assert!(snapshot_matches_pending(&t, &snapshot(&[owned])));
    }

    #[test]
    fn absence_needs_visible_ownership_for_http_but_not_magnets() {
        let http = torrent("https://pt.example/download.php?id=1");
        let magnet = torrent("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let generic_only = vec![snapshot(&[])];
        assert!(!absence_is_provable(&http, &generic_only));
        assert!(absence_is_provable(&magnet, &generic_only));
        // Review M-2: an empty list is no evidence for HTTP torrents.
        assert!(!absence_is_provable(&http, &[]));
        // Review L-8: nor for magnets (client may still be loading torrents).
        assert!(!absence_is_provable(&magnet, &[]));
        let visible = vec![snapshot(&[downloader::ownership_tag("https://x/other")])];
        assert!(absence_is_provable(&http, &visible));
    }
}
