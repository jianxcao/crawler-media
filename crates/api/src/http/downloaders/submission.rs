use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{DownloaderId, Release, Torrent, UserId};
use serde_json::{Value, json};

use super::submission_idempotency::ManualSubmitOutcome;
use super::tasks::SubmitInput;
use crate::http::{err, ok};
use crate::management::ApiState;

fn resolve_subscribe_and_check_owner(
    state: &ApiState,
    subscribe_id_raw: Option<&str>,
    user_id: Option<UserId>,
    torrent: &Torrent,
    parsed: &Release,
) -> Result<Option<domain::Subscribe>, Response> {
    let Some(raw) = subscribe_id_raw else {
        return Ok(None);
    };
    let id = domain::SubscribeId::from_str(raw)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "subscribe.invalid", "订阅 id 无效"))?;
    let loaded = state.store.as_ref().get_subscribe(id);
    let subscribe = match loaded {
        Ok(Some(s)) => {
            if let Some(uid) = user_id {
                let store = state.store.as_ref();
                let is_admin = store.user_role(uid).map(|r| r == "admin").unwrap_or(false);
                if !is_admin && s.user_id != uid {
                    return Err(err(
                        StatusCode::FORBIDDEN,
                        "auth.forbidden",
                        "无权操作他人的订阅",
                    ));
                }
            }
            s
        }
        Ok(None) => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "subscribe.missing",
                "订阅不存在",
            ));
        }
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            ));
        }
    };
    let media = match state.store.as_ref().get_media(subscribe.media_id) {
        Ok(Some(m)) => m,
        Ok(None) => return Err(err(StatusCode::NOT_FOUND, "media.missing", "媒体不存在")),
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            ));
        }
    };
    if !subscribe::candidate_matches_subscribe(&subscribe, &media, parsed) {
        tracing::error!(subscribe_id = %subscribe.id, media = %media.title, torrent = %torrent.title, "手动投递的种子与订阅不匹配");
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscribe.torrent_mismatch",
            "种子与订阅媒体或范围不匹配",
        ));
    }
    Ok(Some(subscribe))
}

fn resolve_downloader_and_save_path(
    state: &ApiState,
    subscribe: Option<&domain::Subscribe>,
    torrent: &Torrent,
    explicit_id: Option<DownloaderId>,
    body_save_path: Option<String>,
    auto_route: bool,
) -> Result<(Option<DownloaderId>, Option<String>, Option<String>), Response> {
    let downloader_id = match explicit_id {
        Some(id)
            if state
                .store
                .as_ref()
                .get_downloader(id)
                .ok()
                .flatten()
                .is_some() =>
        {
            Some(id)
        }
        Some(_) => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "downloader.missing",
                "下载器不存在",
            ));
        }
        None => match crate::delivery::target_id(state, subscribe, torrent) {
            Ok(id) => id,
            Err(e) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "downloader.error",
                    &e.to_string(),
                ));
            }
        },
    };
    let save_path = if auto_route {
        let Some(host_path) = body_save_path.as_deref().filter(|p| !p.trim().is_empty()) else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "download.route",
                "自动入库必须提供预演得到的保存目录",
            ));
        };
        let Some(id) = downloader_id else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "downloader.missing",
                "没有可用的下载器",
            ));
        };
        let maps = match state.store.as_ref().get_downloader(id) {
            Ok(Some(row)) => row.path_maps,
            Ok(None) => {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "downloader.missing",
                    "下载器不存在",
                ));
            }
            Err(e) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                ));
            }
        };
        let path = std::path::Path::new(host_path);
        Some(
            maps.iter()
                .find_map(|m| m.remap_to_downloader(path))
                .unwrap_or_else(|| host_path.to_string()),
        )
    } else {
        body_save_path.clone().filter(|p| !p.trim().is_empty())
    };
    let response_save_path = body_save_path;
    Ok((downloader_id, save_path, response_save_path))
}

pub(super) enum SubmitFailure {
    Conflict,
    Downloader(downloader::DownloaderError),
    Response(Response),
}

/// D-M5: any slot of `parsed` already owned or in flight for `subscribe`.
fn slot_taken(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    parsed: &Release,
) -> Result<(bool, store::SubscribeLibrarySnapshot), String> {
    let (media, pending) = {
        let store = state.store.as_ref();
        let media = store
            .get_media(subscribe.media_id)
            .map_err(|error| error.to_string())?
            .ok_or("Media not found")?;
        let pending = store
            .load_pending(subscribe.id)
            .map_err(|error| error.to_string())?;
        (media, pending)
    };
    let observation = crate::subscribe_library_read::observe_subscribe_library_facts(
        state, subscribe, media.kind,
    )?;
    let releases = crate::worker::finish_search::pending_releases(&pending);
    let facts = subscribe::facts_with_pending(observation.facts, &releases, subscribe, &media);
    let taken = subscribe::pending_slots(subscribe, parsed)
        .iter()
        .any(|(s, e)| facts.get(*s, *e).is_some());
    Ok((taken, observation.snapshot))
}

/// D-M5: score the manual torrent with the subscription's filter (0 if rejected).
fn filter_score(state: &ApiState, subscribe: &domain::Subscribe, torrent: &Torrent) -> i32 {
    let Ok(Some(filter)) = state.store.as_ref().get_filter(subscribe.filter_id) else {
        return 0;
    };
    filter::admit(vec![torrent.clone()], &filter)
        .admitted
        .first()
        .map_or(0, |scored| scored.score)
}

fn build_submit_torrent(body: &SubmitInput) -> (Torrent, Release, Vec<Value>) {
    let torrent = Torrent {
        site_id: body
            .site_id
            .as_deref()
            .and_then(|id| domain::SiteId::from_str(id).ok())
            .unwrap_or_else(|| domain::SiteId::from_uuid(uuid::Uuid::nil())),
        title: body.title.clone(),
        enclosure: body.download_url.clone(),
        size_bytes: body.size_bytes,
        seeders: None,
        free: false,
        hr: None,
        imdb_id: None,
        id: body.torrent_id.clone(),
        leechers: None,
        snatched: None,
        upload_time: body.publish_time.clone(),
        detail_url: None,
        category: body.category.clone(),
        poster_url: None,
        free_2x: false,
        free_until: None,
    };
    let parsed = release::parse(&torrent.title);
    let units = match parsed.covered_episodes().as_slice() {
        [] if parsed.season.is_some() => {
            vec![json!({ "season_number": parsed.season, "episode_number": 0 })]
        }
        [] => vec![json!({ "season_number": 0, "episode_number": 0 })],
        episodes => episodes
            .iter()
            .map(|(season, episode)| json!({ "season_number": season, "episode_number": episode }))
            .collect(),
    };
    (torrent, parsed, units)
}

pub(crate) async fn submit(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<UserId>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SubmitInput>,
) -> Response {
    let http_idem_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // H-1: manual submission writes pending rows.
    if let Err(response) = state.require_writable() {
        return response;
    }
    if body
        .site_id
        .as_deref()
        .is_some_and(|id| domain::SiteId::from_str(id).is_err())
    {
        return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效");
    }
    let (torrent, parsed, units) = build_submit_torrent(&body);
    let explicit_id = match body
        .downloader_id
        .as_deref()
        .map(DownloaderId::from_str)
        .transpose()
    {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || {
            submit_blocking(
                worker,
                user_id,
                body,
                torrent,
                parsed,
                units,
                explicit_id,
                http_idem_key,
            )
        })
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "Manual Downloader submission worker unavailable");
            crate::http::store_wait::busy_response()
        }
    }
}

fn submit_blocking(
    state: ApiState,
    user_id: Option<UserId>,
    body: SubmitInput,
    torrent: Torrent,
    parsed: Release,
    units: Vec<Value>,
    explicit_id: Option<DownloaderId>,
    http_idem_key: Option<String>,
) -> Response {
    if let Err(response) = state
        .require_writable()
        .and_then(|_| state.require_write_admitted())
    {
        return response;
    }
    let subscribe = match resolve_subscribe_and_check_owner(
        &state,
        body.subscribe_id.as_deref(),
        user_id,
        &torrent,
        &parsed,
    ) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (downloader_id, save_path, response_save_path) = match resolve_downloader_and_save_path(
        &state,
        subscribe.as_ref(),
        &torrent,
        explicit_id,
        body.save_path,
        body.auto_route,
    ) {
        Ok(res) => res,
        Err(r) => return r,
    };
    let pin_downloader = explicit_id.is_some() || body.auto_route;
    let delivery = SubmissionDelivery {
        state: state.clone(),
        user_id,
        torrent: torrent.clone(),
        subscribe,
        parsed,
        downloader_id,
        pin_downloader,
        save_path,
        force: body.force,
        http_idem_key,
    };
    finish_manual_submission(
        &state,
        PreparedSubmission {
            delivery,
            units,
            response_save_path,
            category: body.category,
            auto_route: body.auto_route,
        },
    )
}

struct PreparedSubmission {
    delivery: SubmissionDelivery,
    units: Vec<Value>,
    response_save_path: Option<String>,
    category: Option<String>,
    auto_route: bool,
}

fn finish_manual_submission(state: &ApiState, prepared: PreparedSubmission) -> Response {
    let PreparedSubmission {
        delivery,
        units,
        response_save_path,
        category,
        auto_route,
    } = prepared;
    let torrent = delivery.torrent.clone();
    let mut downloader_id = delivery.downloader_id;
    let add_result = delivery.run();

    let add_result = match add_result {
        Err(SubmitFailure::Conflict) => {
            return err(
                StatusCode::CONFLICT,
                "subscribe.slot_taken",
                "该集已在库或正在下载；确认后可用 force=true 强制投递",
            );
        }
        Err(SubmitFailure::Response(response)) => return response,
        Err(SubmitFailure::Downloader(e)) => Err(e),
        Ok(final_dl_id) => {
            downloader_id = final_dl_id;
            Ok(())
        }
    };
    match add_result {
        Ok(_) => {
            if let Some(cat) = category.as_deref().filter(|c| !c.trim().is_empty()) {
                let pref = json!({
                    "kind": if auto_route { "smart" } else if response_save_path.is_some() { "dir" } else { "default" },
                    "save_path": response_save_path,
                    "downloader_id": downloader_id
                        .filter(|id| !id.as_uuid().is_nil())
                        .map(|id| id.to_string()),
                    "downloader_name": Value::Null,
                    "updated_at": crate::store::now_rfc3339(),
                });
                let saved = crate::http::store_wait::with_foreground_store(&state, |store| {
                    let raw = store.get_setting("download.target_prefs")?;
                    let mut prefs = raw
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .unwrap_or_else(|| json!({}));
                    prefs[cat] = pref;
                    store.put_setting("download.target_prefs", &prefs.to_string())
                });
                match saved {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Submitted Torrent target preferences could not be saved")
                    }
                    Err(response) => {
                        tracing::warn!(status = %response.status(), "Submitted Torrent target preferences rejected by write admission");
                        return response;
                    }
                }
            }
            tracing::info!(torrent = %torrent.title, downloader_id = ?downloader_id, "种子已提交给下载器");
            ok(json!({ "ok": true, "save_path": response_save_path, "units": units }))
                .into_response()
        }
        Err(e) => {
            tracing::error!(%e, torrent = %torrent.title, downloader_id = ?downloader_id, "提交种子失败");
            err(
                StatusCode::BAD_REQUEST,
                "downloader.add_failed",
                &e.to_string(),
            )
        }
    }
}

/// One manual delivery serialized with the Subscribe's search and Transfer.
pub(super) struct SubmissionDelivery {
    pub(super) state: ApiState,
    pub(super) user_id: Option<UserId>,
    pub(super) torrent: Torrent,
    pub(super) subscribe: Option<domain::Subscribe>,
    pub(super) parsed: Release,
    pub(super) downloader_id: Option<DownloaderId>,
    pub(super) pin_downloader: bool,
    pub(super) save_path: Option<String>,
    pub(super) force: bool,
    pub(super) http_idem_key: Option<String>,
}

pub(super) fn deleting_subscribe_failure() -> SubmitFailure {
    SubmitFailure::Response(err(
        StatusCode::CONFLICT,
        "subscription.deleting",
        "订阅正在删除",
    ))
}

pub(super) fn subscribe_config_changed(
    current: &domain::Subscribe,
    expected: &domain::Subscribe,
) -> bool {
    current.media_id != expected.media_id
        || current.filter_id != expected.filter_id
        || current.coverage != expected.coverage
        || current.downloader_id != expected.downloader_id
        || current.library_id != expected.library_id
}

fn revalidate_before_submit(
    state: &ApiState,
    subscribe: Option<&domain::Subscribe>,
    snapshot: Option<&store::SubscribeLibrarySnapshot>,
    parsed: &Release,
) -> Result<(), SubmitFailure> {
    let Some(sub) = subscribe else {
        return state
            .require_writable()
            .and_then(|_| state.require_write_admitted())
            .map_err(SubmitFailure::Response);
    };
    let guard = state.subscribe_guard(sub.id);
    let _held = guard.lock();
    if state.subscribe_is_deleting(sub.id) {
        return Err(deleting_subscribe_failure());
    }
    let store = state.store.as_ref();
    let current = store
        .get_subscribe(sub.id)
        .map_err(|error| match error {
            store::StoreError::WriteBusy
            | store::StoreError::ReadBusy
            | store::StoreError::ReadDeadline => {
                SubmitFailure::Response(crate::http::store_wait::busy_response())
            }
            other => SubmitFailure::Response(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &other.to_string(),
            )),
        })?
        .ok_or_else(|| {
            SubmitFailure::Response(err(
                StatusCode::NOT_FOUND,
                "subscribe.missing",
                "订阅不存在",
            ))
        })?;
    if subscribe_config_changed(&current, sub) {
        return Err(SubmitFailure::Response(err(
            StatusCode::CONFLICT,
            "subscription.changed",
            "订阅配置已变更，请刷新后重试",
        )));
    }
    if let Some(snapshot) = snapshot {
        let current_snap = store
            .subscribe_snapshot_is_current(snapshot, &Default::default())
            .map_err(|error| {
                SubmitFailure::Response(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "subscribe.observation_failed",
                    &error.to_string(),
                ))
            })?;
        if !current_snap || pending_slot_taken(store, &current, parsed)? {
            return Err(SubmitFailure::Conflict);
        }
    }
    state
        .require_writable()
        .and_then(|_| state.require_write_admitted())
        .map_err(SubmitFailure::Response)
}

fn pending_slot_taken(
    store: &crate::Store,
    subscribe: &domain::Subscribe,
    parsed: &Release,
) -> Result<bool, SubmitFailure> {
    let media = store
        .get_media(subscribe.media_id)
        .map_err(|error| {
            SubmitFailure::Response(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "subscribe.observation_failed",
                &error.to_string(),
            ))
        })?
        .ok_or_else(|| {
            SubmitFailure::Response(err(StatusCode::NOT_FOUND, "media.missing", "媒体不存在"))
        })?;
    let pending = store.load_pending(subscribe.id).map_err(|error| {
        SubmitFailure::Response(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subscribe.observation_failed",
            &error.to_string(),
        ))
    })?;
    let releases = crate::worker::finish_search::pending_releases(&pending);
    let facts = subscribe::facts_with_pending(
        domain::SubscribeFacts::default(),
        &releases,
        subscribe,
        &media,
    );
    Ok(subscribe::pending_slots(subscribe, parsed)
        .iter()
        .any(|(s, e)| facts.get(*s, *e).is_some()))
}

impl SubmissionDelivery {
    fn preflight_subscribe_guard(&self) -> Result<i32, SubmitFailure> {
        let mut snapshot = None;
        let score = match &self.subscribe {
            Some(sub) => {
                if self.state.subscribe_is_deleting(sub.id) {
                    return Err(deleting_subscribe_failure());
                }
                if !self.force {
                    let (taken, observed) =
                        slot_taken(&self.state, sub, &self.parsed).map_err(|error| {
                            SubmitFailure::Response(err(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "subscribe.observation_failed",
                                &error,
                            ))
                        })?;
                    if taken {
                        return Err(SubmitFailure::Conflict);
                    }
                    snapshot = Some(observed);
                }
                filter_score(&self.state, sub, &self.torrent)
            }
            None => 0,
        };
        revalidate_before_submit(
            &self.state,
            self.subscribe.as_ref(),
            snapshot.as_ref(),
            &self.parsed,
        )?;
        Ok(score)
    }

    pub(super) fn build_manual_plan_and_ctx(
        &self,
        score: i32,
    ) -> Result<
        (
            downloader::FrozenDownloadPlan,
            downloader::DownloadOpContext,
        ),
        SubmitFailure,
    > {
        let source = if self.torrent.site_id.as_uuid().is_nil() {
            downloader::TorrentSource::from_url(&self.torrent.enclosure)
        } else {
            downloader::TorrentSource::from_site_enclosure(&self.torrent.enclosure)
        }
        .map_err(|e| {
            SubmitFailure::Downloader(downloader::DownloaderError::Message(e.to_string()))
        })?;
        let op_id = domain::PluginOperationId::new();
        let plan = downloader::FrozenDownloadPlan {
            torrent: self.torrent.clone(),
            source,
            purpose: downloader::SubmissionPurpose::Manual,
            downloader_id: self.downloader_id.filter(|_| self.pin_downloader),
            frozen_endpoint: None,
            subscribe_id: self.subscribe.as_ref().map(|s| s.id),
            subscribe_generation: None,
            policy_revision: None,
            media_id: self.subscribe.as_ref().map(|s| s.media_id),
            score: Some(score),
            release_override: Some(self.parsed.clone()),
            force: self.force,
            save_path: self.save_path.clone(),
            category: None,
            tags: self
                .subscribe
                .as_ref()
                .map(|s| vec![format!("cm-sub:{}", s.id)])
                .unwrap_or_default(),
            paused: false,
            upload_limit: None,
            download_limit: None,
            sequential: false,
            first_last_piece: false,
            budget_key: None,
            plan_digest: None,
        };
        let idempotency_key = match self.http_idem_key.as_deref() {
            Some(k) => format!("manual-http:{k}"),
            None => format!("manual:{op_id}"),
        };
        let subject = self.user_id.map_or_else(
            || {
                self.subscribe
                    .as_ref()
                    .map_or_else(|| "manual".to_string(), |s| format!("user:{}", s.user_id))
            },
            |u| format!("user:{u}"),
        );
        let op_ctx = downloader::DownloadOpContext {
            operation_id: op_id,
            cause_id: format!("manual:{op_id}"),
            subject,
            idempotency_key,
            plugin_id: None,
            generation: None,
            config_revision: None,
            dry_run: false,
        };
        Ok((plan, op_ctx))
    }

    pub(super) fn subscribe_score(&self) -> i32 {
        self.subscribe
            .as_ref()
            .map_or(0, |sub| filter_score(&self.state, sub, &self.torrent))
    }

    fn run(self) -> Result<Option<DownloaderId>, SubmitFailure> {
        if let Some(replayed_dl_id) = self.replay_http_manual_if_present()? {
            return Ok(replayed_dl_id);
        }
        let score = self.preflight_subscribe_guard()?;
        let (plan, op_ctx) = self.build_manual_plan_and_ctx(score)?;
        let mut svc = crate::download_service::ApiDownloadService::new(self.state.clone());
        if let Some(sub) = self.subscribe.as_ref() {
            svc = svc.with_fence(sub.into());
        }
        let outcome = svc.submit_sync(plan, op_ctx).map_err(|e| match e {
            downloader::DownloadSubmitError::Busy(_) => {
                SubmitFailure::Response(crate::http::store_wait::busy_response())
            }
            downloader::DownloadSubmitError::Cancelled => self.classify_cancelled_submit(),
            other => {
                let msg = self.sanitize_submit_reason(&other.to_string());
                SubmitFailure::Downloader(downloader::DownloaderError::Message(msg))
            }
        })?;
        let info = match outcome {
            downloader::DownloadOutcome::Submitted {
                operation_id,
                downloader_id,
                info_hash,
                already_owned,
                ..
            } => ManualSubmitOutcome {
                operation_id: Some(operation_id),
                dl_id: downloader_id.or(self.downloader_id),
                info_hash,
                is_unknown: false,
                already_owned,
            },
            downloader::DownloadOutcome::Unknown {
                operation_id,
                downloader_id,
                ..
            } => ManualSubmitOutcome {
                operation_id: Some(operation_id),
                dl_id: downloader_id.or(self.downloader_id),
                info_hash: None,
                is_unknown: true,
                already_owned: false,
            },
            downloader::DownloadOutcome::Rejected { reason, .. } => {
                let msg = self.sanitize_submit_reason(&reason);
                return Err(SubmitFailure::Downloader(
                    downloader::DownloaderError::Message(msg),
                ));
            }
            downloader::DownloadOutcome::Deferred { reason, .. } => {
                let msg = self.sanitize_submit_reason(&reason);
                return Err(SubmitFailure::Response(err(
                    StatusCode::CONFLICT,
                    "plugin.deferred",
                    &msg,
                )));
            }
        };
        let final_dl_id = info.dl_id;
        self.persist_or_rollback_pending(info, score)?;
        Ok(final_dl_id)
    }

    pub(super) fn sanitize_submit_reason(&self, raw: &str) -> String {
        let is_admin = self
            .user_id
            .is_some_and(|uid| self.state.store.user_role(uid).is_ok_and(|r| r == "admin"));
        if is_admin {
            hooks::sanitize_reason(raw)
        } else {
            hooks::redact_urls(raw)
        }
    }
}
