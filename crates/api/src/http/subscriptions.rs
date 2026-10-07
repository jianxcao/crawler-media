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
pub(crate) mod snapshot;
mod views;
pub(crate) use views::{coverage_json, list_subscriptions, subscription_json};
mod patch_jobs;

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

fn persist_removed_pending(
    state: &ApiState,
    subscribe_id: SubscribeId,
    torrent: &domain::Torrent,
    errors: &mut Vec<String>,
) -> bool {
    if let Err(error) = state
        .store
        .lock()
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
    let id = match SubscribeId::from_str(raw) {
        Ok(id) => id,
        Err(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "订阅 id 无效",
            ));
        }
    };
    let store = state.store.lock();
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
    let (subscribe, media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let facts = {
        let store = state.store.lock();
        store.load_subscribe_facts(subscribe.id).unwrap_or_default()
    };
    let mut body = {
        let store = state.store.lock();
        subscription_json(&store, state.catalog.as_ref(), &subscribe, &media, &facts)
    };

    // 核心死锁根因：wanted_json 内部会调用 catalog.season_episodes()，
    // 底层的 TmdbHttp 需要实时读取 store.api_key() (即 state.store.lock())。
    // 如果这里在持有着 store 锁的同时调用 catalog，同一线程或跨线程发生锁竞争，必定产生死锁！
    // 因此在调用 catalog 之前，绝对不能持有 store.lock()。
    let air_dates: std::collections::HashMap<u32, Option<String>> =
        match (media.kind, media.tmdb_id.as_deref(), &subscribe.coverage) {
            (domain::MediaKind::Tv, Some(tmdb_id), domain::Coverage::Tv { season, .. }) => state
                .catalog
                .season_episodes(tmdb_id, *season)
                .map(|episodes| {
                    episodes
                        .into_iter()
                        .map(|ep| (ep.episode_number, ep.air_date))
                        .collect()
                })
                .unwrap_or_default(),
            _ => std::collections::HashMap::new(),
        };

    let wanted = {
        let store = state.store.lock();
        crate::http::subscription_depth::wanted_json_with_air_dates(
            &store, &media, &subscribe, &facts, air_dates,
        )
    };
    // 详情接口带工单明细（单元状态现算 + 履历历史）。
    if let Some(obj) = body.as_object_mut() {
        obj.insert("wanted".into(), Value::Array(wanted));
    }
    ok(body).into_response()
}

pub(crate) async fn run_subscription_search(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
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
    match crate::jobs_api::seed_subscribe_search(
        &state.jobs.lock(),
        &subscribe.id.to_string(),
        Some(&media.title),
    ) {
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
