//! Subscriptions (CONTEXT.md **Subscribe**): bind a Media to a coverage
//! range plus search/download policy. Depth endpoints map onto subscribe
//! crate facts (wash-cut states, missing episodes).

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Media, Subscribe, SubscribeId, UserId};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

mod create;
mod library_target;
mod types;
pub(crate) use create::create_subscription;
pub(crate) mod deletion;
pub(crate) use deletion::{TorrentRemovalTarget, delete_subscription};
mod patch;
pub(crate) use patch::patch_subscription;
mod patch_fields;
mod readiness;
pub(crate) use readiness::automation_readiness;
mod removal;

pub(crate) mod poster_view;
mod progress;
pub(crate) mod snapshot;
pub(crate) mod timestamps;
mod views;
pub(crate) use views::{coverage_json, list_subscriptions};

/// 用户隔离：成员只能看到/操作自己的订阅；admin 可见全部。
/// 非归属请求返回 404（不泄露其他用户的订阅是否存在）。
pub(crate) fn ensure_subscribe_owner(
    store: &crate::Store,
    subscribe: &Subscribe,
    user_id: UserId,
) -> Result<(), Response> {
    if subscribe.user_id == user_id {
        return Ok(());
    }
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    if admin {
        return Ok(());
    }
    Err(err(
        StatusCode::NOT_FOUND,
        "subscription.missing",
        "订阅不存在",
    ))
}

pub(crate) fn user_is_admin(store: &crate::Store, user_id: UserId) -> bool {
    store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false)
}

pub(crate) async fn remove_torrents_from_client(
    state: &ApiState,
    targets: Vec<deletion::TorrentRemovalTarget>,
) -> (Vec<domain::Torrent>, usize, Vec<String>) {
    if targets.is_empty() {
        return (Vec::new(), 0, Vec::new());
    }
    let state_clone = state.clone();
    match tokio::task::spawn_blocking(move || remove_targets_blocking(&state_clone, targets)).await
    {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "下载器清理任务失败");
            (Vec::new(), 0, vec![format!("下载器清理任务失败: {error}")])
        }
    }
}

/// One physical Downloader task together with every pending row naming it.
type FrozenClients = HashMap<Option<domain::DownloaderId>, Arc<dyn downloader::Downloader>>;

struct OutstandingTask {
    subscribe_id: SubscribeId,
    key: TaskKey,
    client: Arc<dyn downloader::Downloader>,
    endpoint: Option<String>,
    identity: Option<String>,
    targets: Vec<deletion::TorrentRemovalTarget>,
}

/// How a pending row was matched to a physical task. Rows that share a proven
/// hash are the same task; otherwise only an identical enclosure can be the
/// same submission.
#[derive(Clone, PartialEq, Eq)]
enum TaskKey {
    Proven(String),
    Enclosure(String),
}

fn task_key(identity: Option<&String>, enclosure: &str) -> TaskKey {
    match identity {
        Some(hash) => TaskKey::Proven(hash.clone()),
        None => TaskKey::Enclosure(enclosure.to_string()),
    }
}

/// Resolve every target's proved identity before deleting anything, group the
/// rows that name one physical task, then delete each task exactly once. Doing
/// the resolution first is what lets a second HTTP enclosure of the same task
/// be cleared after the first deletion removed its live ownership mark.
fn remove_targets_blocking(
    state: &ApiState,
    targets: Vec<deletion::TorrentRemovalTarget>,
) -> (Vec<domain::Torrent>, usize, Vec<String>) {
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    let mut pending_cleared = 0;
    let mut deleting_enclosures = HashSet::new();
    let mut frozen_clients = FrozenClients::new();
    let mut tasks: Vec<OutstandingTask> = Vec::new();
    for target in targets {
        match plan_outstanding_target(state, target, &mut frozen_clients, &mut tasks) {
            Ok(planned) => {
                deleting_enclosures.insert(planned);
            }
            Err(error) => errors.push(error),
        }
    }
    for task in tasks {
        remove_one_task(
            state,
            &task,
            &deleting_enclosures,
            &mut removed,
            &mut pending_cleared,
            &mut errors,
        );
    }
    (removed, pending_cleared, errors)
}

fn plan_outstanding_target(
    state: &ApiState,
    target: deletion::TorrentRemovalTarget,
    frozen_clients: &mut FrozenClients,
    tasks: &mut Vec<OutstandingTask>,
) -> Result<(domain::SubscribeId, String), String> {
    let planned = match plan_frozen_client(state, &target, frozen_clients) {
        Ok(planned) => planned,
        Err(error) => {
            tracing::error!(torrent = %target.torrent.title, %error, "无法规划下载任务清理");
            return Err(format!(
                "删除下载任务失败 {}: {error}",
                target.torrent.title
            ));
        }
    };
    let enclosure = target.torrent.enclosure.clone();
    let subscribe_id = target.subscribe_id;
    let key = task_key(planned.identity.as_ref(), &enclosure);
    match tasks.iter_mut().find(|task| {
        task.subscribe_id == subscribe_id && task.endpoint == planned.endpoint && task.key == key
    }) {
        Some(task) => task.targets.push(target),
        None => tasks.push(OutstandingTask {
            subscribe_id,
            key,
            client: planned.client,
            endpoint: planned.endpoint,
            identity: planned.identity,
            targets: vec![target],
        }),
    }
    Ok((subscribe_id, enclosure))
}

struct FrozenPlan {
    client: Arc<dyn downloader::Downloader>,
    endpoint: Option<String>,
    identity: Option<String>,
}

fn plan_frozen_client(
    state: &ApiState,
    target: &deletion::TorrentRemovalTarget,
    frozen_clients: &mut FrozenClients,
) -> Result<FrozenPlan, downloader::DownloaderError> {
    let client = if let Some(client) = frozen_clients.get(&target.downloader_id) {
        client.clone()
    } else {
        let client = crate::delivery::frozen_client_for_id(state, target.downloader_id)?;
        frozen_clients.insert(target.downloader_id, client.clone());
        client
    };
    let endpoint = client.endpoint();
    if endpoint.is_none() {
        tracing::warn!(downloader_id = ?target.downloader_id, "downloader client cannot report its endpoint");
    }
    let identity =
        removal::proven_task_identity(client.as_ref(), &target.torrent)?.ok_or_else(|| {
            downloader::unproven("HTTP enclosure could not prove an actual downloader identity")
        })?;
    Ok(FrozenPlan {
        client,
        endpoint,
        identity: Some(identity),
    })
}

fn remove_one_task(
    state: &ApiState,
    task: &OutstandingTask,
    deleting_enclosures: &HashSet<(domain::SubscribeId, String)>,
    removed: &mut Vec<domain::Torrent>,
    pending_cleared: &mut usize,
    errors: &mut Vec<String>,
) {
    let Some(first) = task.targets.first() else {
        return;
    };
    match removal::is_task_protected(
        state,
        first,
        task.client.as_ref(),
        task.identity.as_deref(),
        task.endpoint.as_deref(),
        deleting_enclosures,
    ) {
        Ok(true) => {
            for target in &task.targets {
                if persist_removed_pending(state, target.subscribe_id, &target.torrent, errors) {
                    *pending_cleared += 1;
                }
            }
        }
        Ok(false) => match delete_planned_task(task, first) {
            Ok(()) => {
                tracing::info!(
                    torrent = %first.torrent.title,
                    hash = ?task.identity,
                    endpoint = ?task.endpoint,
                    pending = task.targets.len(),
                    "已从下载器移除证实归属的任务"
                );
                mark_deleted_task_ownership(state, first, task.identity.as_deref());
                for target in &task.targets {
                    if persist_removed_pending(state, target.subscribe_id, &target.torrent, errors)
                    {
                        removed.push(target.torrent.clone());
                        *pending_cleared += 1;
                    }
                }
            }
            Err(error) => {
                for target in &task.targets {
                    tracing::error!(torrent = %target.torrent.title, %error, "无法安全移除下载任务");
                    errors.push(format!(
                        "删除下载任务失败 {}: {error}",
                        target.torrent.title
                    ));
                }
            }
        },
        Err(error) => {
            for target in &task.targets {
                tracing::error!(torrent = %target.torrent.title, %error, "无法确认下载任务所有权");
                errors.push(format!(
                    "删除下载任务失败 {}: {error}",
                    target.torrent.title
                ));
            }
        }
    }
}

fn delete_planned_task(
    task: &OutstandingTask,
    first: &deletion::TorrentRemovalTarget,
) -> Result<(), downloader::DownloaderError> {
    match task.identity.as_deref() {
        Some(hash) => task.client.delete_task(hash, true),
        None => task.client.remove_owned(&first.torrent, true),
    }
}

pub(crate) fn finalize_deleted_task_by_hash(
    store: &crate::Store,
    dl_id: domain::DownloaderId,
    hash: &str,
    now_utc: i64,
) -> Result<(), String> {
    let Some(norm_hash) = Some(hash.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()) else {
        return Ok(());
    };
    let rows = store
        .find_download_ownership_by_hash(dl_id, &norm_hash)
        .map_err(|error| error.to_string())?;
    for own in rows {
        if own.ownership_state == store::DownloadOwnershipState::Owned {
            store
                .record_download_ownership(&store::RecordDownloadOwnershipInput {
                    downloader_id: dl_id,
                    ownership_tag: own.ownership_tag,
                    info_hash: own.info_hash.or_else(|| Some(norm_hash.clone())),
                    operation_id: own.operation_id,
                    subscribe_id: own.subscribe_id,
                    plugin_id: own.plugin_id,
                    subject: own.subject,
                    site_id: own.site_id,
                    enclosure: own.enclosure,
                    ownership_state: store::DownloadOwnershipState::Deleted,
                    provenance_kind: own.provenance_kind,
                    verified_bytes: own.verified_bytes,
                    bytes_digest: own.bytes_digest,
                    now_utc,
                })
                .map_err(|error| error.to_string())?;
        }
    }
    tombstone_piggybacked_submitted_ops(store, dl_id, &norm_hash, now_utc)?;
    clean_boost_task_facts_for_hash(store, dl_id, &norm_hash, now_utc)
}

fn tombstone_piggybacked_submitted_ops(
    store: &crate::Store,
    dl_id: domain::DownloaderId,
    norm_hash: &str,
    now_utc: i64,
) -> Result<(), String> {
    let filter = store::DownloadOperationFilter {
        downloader_id: Some(dl_id),
        state: Some(store::DownloadOperationState::Submitted),
        ..Default::default()
    };
    let mut matching_ops = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .list_download_operations(&filter, after.as_deref(), Some(100))
            .map_err(|error| error.to_string())?;
        let next = page.next_cursor.clone();
        for op in page.items {
            if op
                .info_hash
                .as_deref()
                .is_some_and(|ih| ih.trim().eq_ignore_ascii_case(norm_hash))
                && matches!(
                    store.get_download_ownership(dl_id, &op.ownership_tag),
                    Ok(None)
                )
            {
                matching_ops.push(op);
            }
        }
        if next.is_none() || next == after {
            break;
        }
        after = next;
    }
    for op in matching_ops {
        store
            .record_download_ownership(&store::RecordDownloadOwnershipInput {
                downloader_id: dl_id,
                ownership_tag: op.ownership_tag,
                info_hash: Some(norm_hash.to_string()),
                operation_id: op.operation_id,
                subscribe_id: op.subscribe_id,
                plugin_id: op.plugin_id,
                subject: op.subject,
                site_id: op.site_id,
                enclosure: op.enclosure,
                ownership_state: store::DownloadOwnershipState::Deleted,
                provenance_kind: store::DownloadProvenanceKind::ExistingOwned,
                verified_bytes: op.verified_bytes,
                bytes_digest: op.bytes_digest,
                now_utc,
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn clean_boost_task_facts_for_hash(
    store: &crate::Store,
    dl_id: domain::DownloaderId,
    norm_hash: &str,
    now_utc: i64,
) -> Result<(), String> {
    let active = store
        .list_active_boost_task_facts()
        .map_err(|error| error.to_string())?;
    for tf in active {
        if tf.downloader_id == dl_id
            && tf.info_hash.trim().eq_ignore_ascii_case(norm_hash)
            && tf.state != store::BoostTaskState::Cleaned
        {
            match store.upsert_boost_task_fact(&store::UpsertBoostTaskFactInput {
                task_fact_id: tf.task_fact_id,
                plugin_id: tf.plugin_id,
                quota_key: tf.quota_key,
                downloader_id: tf.downloader_id,
                ownership_tag: tf.ownership_tag,
                info_hash: tf.info_hash,
                site_id: tf.site_id,
                state: store::BoostTaskState::Cleaned,
                occupied_bytes: 0,
                retained_bytes: 0,
                hr_active: tf.hr_active,
                hr_until: tf.hr_until,
                hold_until: tf.hold_until,
                free_until: tf.free_until,
                protection_state: tf.protection_state,
                protection_reason: tf.protection_reason,
                observed_at: now_utc,
                expected_revision: Some(tf.revision),
                now_utc,
            }) {
                Ok(_)
                | Err(store::StoreError::Plugin(
                    store::plugins::PluginError::Conflict | store::plugins::PluginError::NotFound,
                )) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    Ok(())
}

fn mark_deleted_task_ownership(
    state: &ApiState,
    first: &deletion::TorrentRemovalTarget,
    hash: Option<&str>,
) {
    let Some(hash) = hash.map(str::trim).filter(|h| !h.is_empty()) else {
        return;
    };
    let dl_id = first
        .downloader_id
        .or_else(|| {
            state
                .store
                .default_downloader()
                .ok()
                .flatten()
                .map(|row| row.id)
        })
        .or_else(|| {
            crate::delivery::target_id_from_store(&state.store, None, &first.torrent)
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| domain::DownloaderId::from_uuid(uuid::Uuid::nil()));
    if let Err(error) =
        finalize_deleted_task_by_hash(&state.store, dl_id, hash, crate::job_loop::unix_now())
    {
        tracing::warn!(%error, downloader_id = %dl_id, info_hash = %hash, "标记下载任务归属为已删除失败");
    }
}

fn persist_removed_pending(
    state: &ApiState,
    subscribe_id: SubscribeId,
    torrent: &domain::Torrent,
    errors: &mut Vec<String>,
) -> bool {
    if let Err(error) = state
        .store
        .as_ref()
        .delete_pending(subscribe_id, &torrent.enclosure)
    {
        tracing::error!(
            %error,
            subscribe_id = %subscribe_id,
            enclosure = %torrent.enclosure,
            "无法记录已删除的下载任务归属"
        );
        errors.push(format!("记录已删除下载任务失败 {}: {error}", torrent.title));
        return false;
    }
    true
}

pub(crate) fn resolve_subscribe(
    state: &ApiState,
    raw: &str,
    user_id: UserId,
) -> Result<(Subscribe, Media), Response> {
    let _id = match SubscribeId::from_str(raw) {
        Ok(id) => id,
        Err(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "订阅 id 无效",
            ));
        }
    };
    let store = state.store.clone();
    resolve_subscribe_on(&store, raw, user_id)
}

pub(crate) fn resolve_subscribe_on(
    store: &crate::Store,
    raw: &str,
    user_id: UserId,
) -> Result<(Subscribe, Media), Response> {
    let id = SubscribeId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "订阅 id 无效",
        )
    })?;
    let Some(subscribe) = store.get_subscribe(id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    };
    ensure_subscribe_owner(&store, &subscribe, user_id)?;
    let Some(media) = store.get_media(subscribe.media_id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅关联的影视不存在",
        ));
    };
    Ok((subscribe, media))
}

pub(crate) async fn get_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    let capture_id = id.clone();
    let captured = super::store_wait::store_read(&state, &[store::Db::Library, store::Db::App, store::Db::Subscribe, store::Db::Cache], move |store| {
        let (subscribe, media) = resolve_subscribe_on(store, &capture_id, user_id)?;
        let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
        let wanted = crate::http::subscription_depth::wanted::capture(store, &media, &subscribe)
            .map_err(|error| { tracing::error!(%error, subscribe_id=%subscribe.id, "Subscribe detail capture failed");
                err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()) })?;
        Ok((
            snapshot::load_single_subscription_view(store, &subscribe, &media, &facts).map_err(|error| { tracing::error!(%error, "Subscribe timestamp read failed"); err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()) })?,
            wanted,
        ))
    })
    .await;
    let (snapshot, wanted) = match captured {
        Ok(Ok(captured)) => captured,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };
    let media_id = snapshot.media.id;
    let files = state.file_stats.clone();
    let catalog = state.catalog.clone();
    let body = match state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            let air_dates = match (
                snapshot.media.kind,
                snapshot.media.tmdb_id.as_deref(),
                &snapshot.subscribe.coverage,
            ) {
                (domain::MediaKind::Tv, Some(tmdb), domain::Coverage::Tv { season, .. }) => catalog
                    .cached_season_episodes(tmdb, *season)
                    .map(|episodes| {
                        let episodes = episodes.unwrap_or_default();
                        episodes
                            .into_iter()
                            .map(|ep| (ep.episode_number, ep.air_date))
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => std::collections::HashMap::new(),
            };
            let mut body = views::render_subscription_snapshot(&snapshot, files.as_ref());
            let wanted = crate::http::subscription_depth::wanted::render(
                wanted,
                &snapshot.media,
                &snapshot.subscribe,
                &snapshot.facts,
                air_dates,
            );
            if let Some(object) = body.as_object_mut() {
                object.insert("wanted".into(), Value::Array(wanted));
            }
            body
        })
        .await
    {
        Ok(body) => body,
        Err(error) => return super::library::browse_work_error(error),
    };
    match super::store_wait::store_read(
        &state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        move |store| {
            let (current, _) = resolve_subscribe_on(store, &id, user_id)?;
            if current.media_id != media_id {
                return Err(err(
                    StatusCode::CONFLICT,
                    "subscription.changed",
                    "订阅身份在读取期间变化，请刷新重试",
                ));
            }
            Ok(())
        },
    )
    .await
    {
        Ok(Ok(())) => ok(body).into_response(),
        Ok(Err(response)) => response,
        Err(error) => error.response(),
    }
}

/// Run a handler body that takes the Subscribe guard (a parking_lot lock that a
/// Transfer may hold across downloader IO) on the bounded blocking budget, never on
/// an async core thread (C02).
pub(crate) async fn guarded_blocking(
    state: &ApiState,
    work: impl FnOnce() -> Response + Send + 'static,
) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || {
            if let Err(response) = worker
                .require_writable()
                .and_then(|_| worker.require_write_admitted())
            {
                return response;
            }
            super::store_wait::with_write_admission(&worker, work)
                .unwrap_or_else(|response| response)
        })
        .await
    {
        Ok(response) => response,
        Err(crate::blocking_budget::BlockingError::Busy) => super::store_wait::busy_response(),
        Err(error) => {
            tracing::error!(%error, "订阅操作的阻塞工作失败");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker_failed",
                &error.to_string(),
            )
        }
    }
}

pub(crate) async fn run_subscription_search(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    let blocking = state.clone();
    guarded_blocking(&blocking, move || {
        subscription_search_blocking(&state, user_id, &id)
    })
    .await
}

fn subscription_search_blocking(state: &ApiState, user_id: UserId, id: &str) -> Response {
    let state = state.clone();
    let id = id.to_string();
    let (initial, _) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let guard = state.subscribe_guard(initial.id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(initial.id) {
        return err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除，无法搜索",
        );
    }
    let (subscribe, media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if subscribe.tracking_state == "paused" {
        return err(
            StatusCode::CONFLICT,
            "subscription.paused",
            "订阅已暂停，无法搜索",
        );
    }
    let queue = match state.jobs.acquire() {
        Ok(queue) => queue,
        Err(error) => return crate::http::jobs::jobs_acquire_response(error),
    };
    match crate::jobs_api::seed_subscribe_search(&queue, subscribe.id, Some(&media.title)) {
        Ok(_) => ok(json!({ "queued": true })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.seed_failed",
            &error.to_string(),
        ),
    }
}

mod forecast;
pub(crate) use forecast::{parse_upload_time, release_forecast};
