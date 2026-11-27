use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use domain::{DownloaderId, PluginOperationId, Release, Subscribe, SubscribeId, Torrent};
use serde::Deserialize;
use serde_json::{Value, json};

mod delivery;
use delivery::ManualAdmitGuard;
pub(crate) use delivery::ensure_subscribe_library_available;

use crate::management::{ApiError, ApiState};

#[derive(Deserialize)]
pub struct AdmitBody {
    subscribe_id: String,
    enclosure: String,
    #[serde(default)]
    release: Option<ReleaseOverride>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct ReleaseOverride {
    title: Option<String>,
    year: Option<u16>,
    season: Option<u32>,
    episode: Option<u32>,
    resolution: Option<String>,
    source: Option<String>,
    codec: Option<String>,
    hdr: Option<String>,
}

struct AdmitCandidate {
    scored: filter::ScoredTorrent,
    corrected: Release,
    override_for_pending: Option<Release>,
    generation: u64,
}

fn conflict_resp(code: &'static str, msg: &str) -> Response {
    ApiError::with(StatusCode::CONFLICT, code, msg.into()).into_response()
}

fn admit_ok_json(subscribe_id: SubscribeId, scored: &filter::ScoredTorrent) -> Json<Value> {
    Json(json!({
        "subscribe_id": subscribe_id.to_string(),
        "title": scored.torrent.title,
        "enclosure": scored.torrent.enclosure,
        "score": scored.score,
    }))
}

fn apply_admit_release_override(base: Release, override_rel: &ReleaseOverride) -> Release {
    Release {
        title: override_rel.title.clone().unwrap_or(base.title),
        year: override_rel.year.or(base.year),
        season: override_rel.season.or(base.season),
        episode: override_rel.episode.or(base.episode),
        episode_to: base.episode_to,
        resolution: override_rel.resolution.clone().or(base.resolution),
        source: override_rel.source.clone().or(base.source),
        codec: override_rel.codec.clone().or(base.codec),
        hdr: override_rel.hdr.clone().or(base.hdr),
        subtitle_language: base.subtitle_language,
        audio_language: base.audio_language,
        group: base.group,
        confidence: domain::Confidence::High,
        season_inferred: false,
    }
}

fn load_admit_context(
    store: &store::Store,
    id: SubscribeId,
) -> Result<(Subscribe, domain::Media, domain::Filter, Vec<domain::Site>), ApiError> {
    let subscribe = store
        .get_subscribe(id)?
        .ok_or_else(|| ApiError::missing("subscribe.missing", "Subscribe not found".into()))?;
    let media = store
        .get_media(subscribe.media_id)?
        .ok_or_else(|| ApiError::missing("media.missing", "Media not found".into()))?;
    let filter = store
        .get_filter(subscribe.filter_id)?
        .ok_or_else(|| ApiError::missing("filter.missing", "Filter not found".into()))?;
    let sites = store.list_enabled_sites()?;
    Ok((subscribe, media, filter, sites))
}

pub async fn admit_torrent(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<AdmitBody>,
) -> Result<Json<Value>, Response> {
    let http_idem_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let budget = state.http_work.clone();
    budget
        .run_to_completion(move || admit_blocking(state, body, http_idem_key))
        .await
        .map_err(|error| match error {
            crate::blocking_budget::BlockingError::Busy => crate::http::store_wait::busy_response(),
            error => {
                tracing::error!(%error, "Manual Torrent admission worker failed");
                ApiError::internal(error.to_string()).into_response()
            }
        })?
}

fn load_guarded_admit_context(
    state: &ApiState,
    subscribe_id: SubscribeId,
    guard: &parking_lot::Mutex<()>,
) -> Result<(Subscribe, domain::Media, domain::Filter, Vec<domain::Site>), Response> {
    let _lease = guard
        .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        .ok_or_else(crate::http::store_wait::busy_response)?;
    if state.subscribe_is_deleting(subscribe_id) {
        return Err(conflict_resp("subscription.deleting", "订阅正在删除"));
    }
    admitted_store(state, |store| load_admit_context(store, subscribe_id))
}

fn find_and_score_admit_candidate(
    state: &ApiState,
    body: &AdmitBody,
    media: &domain::Media,
    filter: &domain::Filter,
    sites: &[domain::Site],
) -> Result<AdmitCandidate, Response> {
    let keyword = media
        .original_title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&media.title);
    let snapshot = state.indexer.snapshot();
    let outcome = state
        .indexer
        .search_with_snapshot(&snapshot, sites, keyword);
    let Some(chosen) = outcome
        .torrents
        .into_iter()
        .find(|torrent| torrent.enclosure == body.enclosure)
    else {
        return Err(
            ApiError::invalid("torrent.missing", "Torrent not found".into()).into_response(),
        );
    };
    let mut corrected = release::parse(&chosen.title);
    if let Some(override_release) = &body.release {
        corrected = apply_admit_release_override(corrected, override_release);
    }
    let override_for_pending = body.release.as_ref().map(|_| corrected.clone());
    let admitted = filter::admit_scored(vec![(chosen, Some(corrected.clone()))], filter).admitted;
    let Some(scored) = admitted.into_iter().next() else {
        return Err(
            ApiError::invalid("subscribe.rejected", "Torrent rejected by Filter".into())
                .into_response(),
        );
    };
    Ok(AdmitCandidate {
        scored,
        corrected,
        override_for_pending,
        generation: outcome.generation,
    })
}

fn build_admit_plan(
    subscribe: &Subscribe,
    scored: &filter::ScoredTorrent,
    corrected: Release,
    delivery_id: Option<DownloaderId>,
    source: downloader::TorrentSource,
) -> downloader::FrozenDownloadPlan {
    downloader::FrozenDownloadPlan {
        torrent: scored.torrent.clone(),
        source,
        purpose: downloader::SubmissionPurpose::Admit,
        downloader_id: delivery_id,
        frozen_endpoint: None,
        subscribe_id: Some(subscribe.id),
        subscribe_generation: None,
        policy_revision: None,
        media_id: Some(subscribe.media_id),
        score: Some(scored.score),
        release_override: Some(corrected),
        force: false,
        save_path: None,
        category: None,
        tags: vec![format!("cm-sub:{}", subscribe.id)],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    }
}

fn ensure_admit_subscribe_unchanged(
    store: &store::Store,
    expected: &Subscribe,
) -> Result<(), ApiError> {
    let current = store
        .get_subscribe(expected.id)?
        .ok_or_else(|| ApiError::missing("subscribe.missing", "Subscribe not found".into()))?;
    if subscribe_config_changed(&current, expected) {
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "subscription.changed",
            "订阅配置已变更，请刷新后重试".into(),
        ));
    }
    Ok(())
}

fn ensure_replayed_admit_pending(
    state: &ApiState,
    guard_to_lock: Option<&parking_lot::Mutex<()>>,
    subscribe: &Subscribe,
    cand: &AdmitCandidate,
    dl_id: Option<DownloaderId>,
    info_hash: Option<&str>,
    is_unknown: bool,
) -> Result<(), Response> {
    let enc = &cand.scored.torrent.enclosure;
    let has_row = |rows: &[(i32, crate::store::PendingDownload)]| {
        rows.iter()
            .any(|(_, p)| &p.torrent.enclosure == enc && (is_unknown || p.submitted_at.is_some()))
    };
    let _lease = match guard_to_lock {
        Some(g) => Some(
            g.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
                .ok_or_else(crate::http::store_wait::busy_response)?,
        ),
        None => None,
    };
    if state.subscribe_is_deleting(subscribe.id) {
        return Err(conflict_resp("subscription.deleting", "订阅正在删除"));
    }
    ensure_subscribe_library_available(state, subscribe)?;
    if state
        .store
        .load_pending(subscribe.id)
        .is_ok_and(|r| has_row(&r))
    {
        return Ok(());
    }
    admitted_store(state, |store| {
        ensure_admit_subscribe_unchanged(store, subscribe)?;
        if has_row(&store.load_pending(subscribe.id)?) {
            return Ok(());
        }
        let now = crate::job_loop::unix_now();
        if !is_unknown
            && store
                .confirm_pending_submission(subscribe.id, enc, now, dl_id, info_hash)
                .is_ok()
        {
            return Ok(());
        }
        let pending = crate::store::PendingDownload {
            submitted_at: Some(if is_unknown { 0 } else { now }),
            torrent: cand.scored.torrent.clone(),
            release_override: cand.override_for_pending.clone(),
            downloader_id: dl_id,
        };
        store.record_manual_pending_submissions(subscribe.id, &[(cand.scored.score, pending)])?;
        if let Some(hash) = info_hash.map(str::trim).filter(|s| !s.is_empty()) {
            store.set_pending_info_hash(subscribe.id, enc, hash)?;
        }
        Ok(())
    })
}

fn find_active_admit_op(
    state: &ApiState,
    subject: &str,
    base_key: &str,
    expected_digest: &str,
) -> Result<Option<store::DownloadOperationRecord>, Response> {
    for attempt in 0usize.. {
        let candidate_key = if attempt == 0 {
            base_key.to_string()
        } else {
            format!("{base_key}#rb:{attempt}")
        };
        let row = match state
            .store
            .get_download_operation_by_idempotency(subject, &candidate_key)
        {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(None),
            Err(
                store::StoreError::WriteBusy
                | store::StoreError::ReadBusy
                | store::StoreError::ReadDeadline,
            ) => return Err(crate::http::store_wait::busy_response()),
            Err(error) => return Err(ApiError::from(error).into_response()),
        };
        if row.request_digest != expected_digest {
            return Err(ApiError::invalid(
                "subscribe.invalid",
                "persistence error: idempotency key reused for a different plugin request".into(),
            )
            .into_response());
        }
        if !crate::download_service::is_operation_rolled_back(state, &row) {
            return Ok(Some(row));
        }
    }
    unreachable!()
}

fn replay_http_admit_if_present(
    state: &ApiState,
    guard_to_lock: Option<&parking_lot::Mutex<()>>,
    subscribe: &Subscribe,
    http_idem_key: Option<&str>,
    cand: &AdmitCandidate,
) -> Result<Option<Json<Value>>, Response> {
    let Some(k) = http_idem_key else {
        return Ok(None);
    };
    let delivery_id =
        crate::delivery::target_id_from_store(&state.store, Some(subscribe), &cand.scored.torrent)
            .ok()
            .flatten();
    let initial_dl_id = delivery_id.unwrap_or_else(|| DownloaderId::from_uuid(uuid::Uuid::nil()));
    let stub = downloader::TorrentSource::Magnet {
        uri: cand.scored.torrent.enclosure.clone(),
    };
    let plan_stub = build_admit_plan(
        subscribe,
        &cand.scored,
        cand.corrected.clone(),
        subscribe.downloader_id,
        stub,
    );
    let expected_digest =
        crate::download_service::compute_request_digest(&plan_stub, initial_dl_id);
    let subject = format!("user:{}", subscribe.user_id);
    let full_key = format!("admit-http:{k}");
    let Some(existing) = find_active_admit_op(state, &subject, &full_key, &expected_digest)? else {
        return Ok(None);
    };
    let Some(outcome) = crate::download_service::replay_outcome(&existing) else {
        return Ok(None);
    };
    let (dl_id, info_hash, is_unknown) = match outcome {
        downloader::DownloadOutcome::Submitted {
            downloader_id,
            info_hash,
            ..
        } => (downloader_id.or(delivery_id), info_hash, false),
        downloader::DownloadOutcome::Unknown { downloader_id, .. } => {
            (downloader_id.or(delivery_id), None, true)
        }
        downloader::DownloadOutcome::Rejected { reason, .. } => {
            return Err(
                ApiError::with(StatusCode::FORBIDDEN, "plugin.rejected", reason).into_response(),
            );
        }
        downloader::DownloadOutcome::Deferred { reason, .. } => {
            return Err(conflict_resp("plugin.deferred", &reason));
        }
    };
    ensure_replayed_admit_pending(
        state,
        guard_to_lock,
        subscribe,
        cand,
        dl_id,
        info_hash.as_deref(),
        is_unknown,
    )?;
    Ok(Some(admit_ok_json(subscribe.id, &cand.scored)))
}

fn record_admit_intent(
    state: &ApiState,
    guard: &parking_lot::Mutex<()>,
    subscribe: &Subscribe,
    cand: &AdmitCandidate,
    downloader_id: Option<DownloaderId>,
    http_idem_key: Option<&str>,
) -> Result<Option<Json<Value>>, Response> {
    let _lease = guard
        .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        .ok_or_else(crate::http::store_wait::busy_response)?;
    if state.subscribe_is_deleting(subscribe.id) {
        return Err(conflict_resp("subscription.deleting", "订阅正在删除"));
    }
    if let Some(replayed) =
        replay_http_admit_if_present(state, None, subscribe, http_idem_key, cand)?
    {
        return Ok(Some(replayed));
    }
    let pending = crate::store::PendingDownload {
        submitted_at: Some(0),
        torrent: cand.scored.torrent.clone(),
        release_override: cand.override_for_pending.clone(),
        downloader_id,
    };
    admitted_store(state, |store| {
        ensure_admit_subscribe_unchanged(store, subscribe)?;
        if store
            .load_pending(subscribe.id)?
            .into_iter()
            .any(|(_, item)| {
                item.torrent.enclosure == pending.torrent.enclosure && item.submitted_at.is_some()
            })
        {
            return Err(ApiError::with(
                StatusCode::CONFLICT,
                "subscription.already_pending",
                "该资源已在下载队列中".into(),
            ));
        }
        store
            .record_manual_pending_submissions(subscribe.id, &[(cand.scored.score, pending)])
            .map_err(ApiError::from)
    })?;
    Ok(None)
}

fn cleanup_unconfirmed_admit_intent(
    state: &ApiState,
    subscribe_id: SubscribeId,
    enclosure: &str,
    mark_rejected: bool,
) {
    let res = if mark_rejected {
        state
            .store
            .mark_pending_rejected(subscribe_id, &[enclosure.to_string()])
    } else {
        state
            .store
            .delete_unconfirmed_pending(subscribe_id, enclosure)
    };
    if let Err(err) = res {
        tracing::warn!(%err, %subscribe_id, "failed to clean up unconfirmed admit pending row");
    }
}

fn subscribe_config_changed(current: &Subscribe, expected: &Subscribe) -> bool {
    current.media_id != expected.media_id
        || current.filter_id != expected.filter_id
        || current.downloader_id != expected.downloader_id
        || current.coverage != expected.coverage
        || current.library_id != expected.library_id
}

fn rollback_admit_after_race(
    state: &ApiState,
    subscribe_id: SubscribeId,
    operation_id: Option<PluginOperationId>,
    torrent: &Torrent,
    dl_id: Option<DownloaderId>,
    info_hash: Option<&str>,
    already_owned: bool,
) {
    if !already_owned {
        if let Ok(client) = crate::delivery::client_for_id(state, dl_id) {
            let del_res = match info_hash.map(str::trim).filter(|s| !s.is_empty()) {
                Some(hash) => match client.delete_task(hash, false) {
                    Err(downloader::DownloaderError::Message(ref m))
                        if m == "delete_task not supported" =>
                    {
                        client.remove_owned(torrent, false)
                    }
                    other => other,
                },
                None => client.remove_owned(torrent, false),
            };
            if let Err(error) = del_res {
                tracing::error!(%error, torrent = %torrent.title, "Failed to roll back manual admit delivery");
            }
        }
        if let Some(op_id) = operation_id
            && let Ok(Some(op_row)) = state.store.get_download_operation(&op_id.to_string())
            && let Some(rec_dl_id) = op_row.downloader_id.or(dl_id)
        {
            let _ = state
                .store
                .record_download_ownership(&store::RecordDownloadOwnershipInput {
                    downloader_id: rec_dl_id,
                    ownership_tag: op_row.ownership_tag,
                    info_hash: op_row.info_hash.or_else(|| info_hash.map(str::to_string)),
                    operation_id: op_row.operation_id,
                    subscribe_id: op_row.subscribe_id,
                    plugin_id: op_row.plugin_id,
                    subject: op_row.subject,
                    site_id: op_row.site_id,
                    enclosure: op_row.enclosure,
                    ownership_state: store::DownloadOwnershipState::Deleted,
                    provenance_kind: store::DownloadProvenanceKind::CreatedFresh,
                    verified_bytes: op_row.verified_bytes,
                    bytes_digest: op_row.bytes_digest,
                    now_utc: crate::job_loop::unix_now(),
                });
        }
    }
    cleanup_unconfirmed_admit_intent(state, subscribe_id, &torrent.enclosure, false);
}

fn guard_admit_subscribe_post_submit(
    state: &ApiState,
    expected: &Subscribe,
) -> Result<Subscribe, Response> {
    if state.subscribe_is_deleting(expected.id) {
        return Err(conflict_resp("subscription.deleting", "订阅正在删除"));
    }
    let cur = match state.store.get_subscribe(expected.id) {
        Ok(Some(cur)) => cur,
        Ok(None) => return Err(conflict_resp("subscription.deleting", "订阅正在删除")),
        Err(
            store::StoreError::WriteBusy
            | store::StoreError::ReadBusy
            | store::StoreError::ReadDeadline,
        ) => return Err(crate::http::store_wait::busy_response()),
        Err(error) => return Err(ApiError::from(error).into_response()),
    };
    if subscribe_config_changed(&cur, expected) {
        return Err(conflict_resp(
            "subscription.changed",
            "订阅配置已变更，请刷新后重试",
        ));
    }
    ensure_subscribe_library_available(state, &cur)?;
    Ok(cur)
}

#[allow(clippy::too_many_arguments)]
fn finalize_submitted_admit(
    state: &ApiState,
    subscribe: &Subscribe,
    scored: &filter::ScoredTorrent,
    operation_id: Option<PluginOperationId>,
    dl_id: Option<DownloaderId>,
    info_hash: Option<&str>,
    already_owned: bool,
    http_idem_key: Option<&str>,
) -> Result<(), Response> {
    let guard = state.subscribe_guard(subscribe.id);
    let held = guard.lock();
    if let Err(response) = guard_admit_subscribe_post_submit(state, subscribe) {
        drop(held);
        rollback_admit_after_race(
            state,
            subscribe.id,
            operation_id,
            &scored.torrent,
            dl_id,
            info_hash,
            already_owned,
        );
        return Err(response);
    }
    let confirm_res = state.store.confirm_pending_submission(
        subscribe.id,
        &scored.torrent.enclosure,
        crate::job_loop::unix_now(),
        dl_id,
        info_hash,
    );
    drop(held);
    if let Err(error) = confirm_res {
        let already_confirmed = http_idem_key.is_some()
            && matches!(error, store::StoreError::Protected(_))
            && state.store.load_pending(subscribe.id).is_ok_and(|rows| {
                rows.iter().any(|(_, p)| {
                    p.torrent.enclosure == scored.torrent.enclosure && p.submitted_at.is_some()
                })
            });
        if !already_confirmed {
            tracing::error!(%error, subscribe_id=%subscribe.id, "Submitted manual Torrent retains unconfirmed pending intent");
            return Err(ApiError::from(error).into_response());
        }
    }
    Ok(())
}

fn map_admit_submit_error(
    state: &ApiState,
    subscribe: &Subscribe,
    enclosure: &str,
    err: downloader::DownloadSubmitError,
) -> Response {
    let subscribe_id = subscribe.id;
    if !matches!(err, downloader::DownloadSubmitError::Conflict(_)) {
        cleanup_unconfirmed_admit_intent(state, subscribe_id, enclosure, false);
    }
    match err {
        downloader::DownloadSubmitError::Busy(_) => crate::http::store_wait::busy_response(),
        downloader::DownloadSubmitError::Cancelled => {
            let guard = state.subscribe_guard(subscribe_id);
            let _held = guard.lock();
            if state.subscribe_is_deleting(subscribe_id) {
                return conflict_resp("subscription.deleting", "订阅正在删除");
            }
            let cur = match state.store.get_subscribe(subscribe_id) {
                Ok(Some(cur)) => cur,
                Ok(None) => return conflict_resp("subscription.deleting", "订阅正在删除"),
                Err(
                    store::StoreError::WriteBusy
                    | store::StoreError::ReadBusy
                    | store::StoreError::ReadDeadline,
                ) => return crate::http::store_wait::busy_response(),
                Err(error) => return ApiError::from(error).into_response(),
            };
            if subscribe_config_changed(&cur, subscribe) || &cur != subscribe {
                return conflict_resp("subscription.changed", "订阅配置已变更，请刷新后重试");
            }
            match ensure_subscribe_library_available(state, &cur) {
                Err(response) => response,
                Ok(()) => conflict_resp("library.unavailable", "目标媒体库不可写入"),
            }
        }
        other => ApiError::invalid("subscribe.invalid", other.to_string()).into_response(),
    }
}

fn submit_and_finalize_admit(
    state: &ApiState,
    subscribe: &Subscribe,
    cand: &AdmitCandidate,
    delivery_id: Option<DownloaderId>,
    source: downloader::TorrentSource,
    http_idem_key: Option<&str>,
) -> Result<(), Response> {
    let op_id = PluginOperationId::new();
    let plan = build_admit_plan(
        subscribe,
        &cand.scored,
        cand.corrected.clone(),
        subscribe.downloader_id,
        source,
    );
    let idempotency_key = match http_idem_key {
        Some(k) => format!("admit-http:{k}"),
        None => format!("admit:{}:{op_id}", subscribe.id),
    };
    let op_ctx = downloader::DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("admit:{}", subscribe.id),
        subject: format!("user:{}", subscribe.user_id),
        idempotency_key,
        plugin_id: None,
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    let enc = &cand.scored.torrent.enclosure;
    let dl_outcome = crate::download_service::ApiDownloadService::new(state.clone())
        .with_expected_generation(Some(cand.generation))
        .with_fence(subscribe.into())
        .submit_sync(plan, op_ctx)
        .map_err(|e| map_admit_submit_error(state, subscribe, enc, e))?;
    match dl_outcome {
        downloader::DownloadOutcome::Submitted {
            operation_id,
            downloader_id,
            info_hash,
            already_owned,
            ..
        } => finalize_submitted_admit(
            state,
            subscribe,
            &cand.scored,
            Some(operation_id),
            downloader_id.or(delivery_id),
            info_hash.as_deref(),
            already_owned,
            http_idem_key,
        ),
        downloader::DownloadOutcome::Unknown { downloader_id, .. } => {
            let guard = state.subscribe_guard(subscribe.id);
            let held = guard.lock();
            if let Err(response) = guard_admit_subscribe_post_submit(state, subscribe) {
                drop(held);
                cleanup_unconfirmed_admit_intent(state, subscribe.id, enc, false);
                return Err(response);
            }
            if let Some(dl_id) = downloader_id
                && let Err(error) = state.store.set_pending_downloader(subscribe.id, enc, dl_id)
            {
                tracing::warn!(%error, subscribe_id=%subscribe.id, "failed to update Unknown pending downloader_id");
            }
            drop(held);
            Ok(())
        }
        downloader::DownloadOutcome::Rejected {
            reason,
            broken_task_hash,
            ..
        } => {
            cleanup_unconfirmed_admit_intent(state, subscribe.id, enc, broken_task_hash.is_some());
            Err(ApiError::with(StatusCode::FORBIDDEN, "plugin.rejected", reason).into_response())
        }
        downloader::DownloadOutcome::Deferred { reason, .. } => {
            cleanup_unconfirmed_admit_intent(state, subscribe.id, enc, false);
            Err(conflict_resp("plugin.deferred", &reason))
        }
    }
}

fn admit_blocking(
    state: ApiState,
    body: AdmitBody,
    http_idem_key: Option<String>,
) -> Result<Json<Value>, Response> {
    let subscribe_id = body.subscribe_id.parse::<SubscribeId>().map_err(|_| {
        ApiError::invalid("subscribe.invalid", "invalid Subscribe id".into()).into_response()
    })?;
    let guard = state.subscribe_guard(subscribe_id);
    let (subscribe, media, filter, sites) =
        load_guarded_admit_context(&state, subscribe_id, &guard)?;
    let cand = find_and_score_admit_candidate(&state, &body, &media, &filter, &sites)?;
    if let Some(replayed) = replay_http_admit_if_present(
        &state,
        Some(&guard),
        &subscribe,
        http_idem_key.as_deref(),
        &cand,
    )? {
        return Ok(replayed);
    }
    let _inflight =
        ManualAdmitGuard::acquire(&state, subscribe.id, &cand.scored.torrent.enclosure)?;
    let delivery = delivery::prepare(&state, &subscribe, &cand.scored.torrent)?;
    let source = downloader::TorrentSource::from_site_enclosure(&cand.scored.torrent.enclosure)
        .map_err(|e| ApiError::invalid("subscribe.invalid", e.to_string()).into_response())?;
    if let Some(replayed) = record_admit_intent(
        &state,
        &guard,
        &subscribe,
        &cand,
        delivery.id,
        http_idem_key.as_deref(),
    )? {
        return Ok(replayed);
    }
    submit_and_finalize_admit(
        &state,
        &subscribe,
        &cand,
        delivery.id,
        source,
        http_idem_key.as_deref(),
    )?;
    tracing::info!(subscribe_id=%subscribe.id, title=%cand.scored.torrent.title, score=cand.scored.score, "Manual Torrent admitted and submitted");
    Ok(admit_ok_json(subscribe.id, &cand.scored))
}

pub async fn admit_torrent_v1(
    state: State<ApiState>,
    headers: HeaderMap,
    body: Json<AdmitBody>,
) -> Response {
    match admit_torrent(state, headers, body).await {
        Ok(Json(data)) => crate::http::ok(data).into_response(),
        Err(error) => error.into_response(),
    }
}

fn admitted_store<T>(
    state: &ApiState,
    work: impl FnOnce(&crate::Store) -> Result<T, ApiError>,
) -> Result<T, Response> {
    crate::http::store_wait::with_foreground_store(state, |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        work(store).map_err(IntoResponse::into_response)
    })?
}
