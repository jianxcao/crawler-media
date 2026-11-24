use std::str::FromStr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId,
};

use super::types::*;
use crate::http::{err, ok};
use crate::management::ApiState;

struct PreparedSubscription {
    user_id: UserId,
    incoming: Media,
    fetch_mode: FetchMode,
    coverage: Coverage,
    explicit_library_id: Option<String>,
    auto_library: Option<store::Library>,
    body: CreateSubscriptionInput,
}

pub(crate) async fn create_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    crate::http::strict_json::StrictJson(body): crate::http::strict_json::StrictJson<
        CreateSubscriptionInput,
    >,
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
            let prepared = match prepare_subscription(&worker, user_id, body) {
                Ok(prepared) => prepared,
                Err(response) => return response,
            };
            let persist_state = worker.clone();
            crate::http::store_wait::with_write_admission(&worker, move || {
                persist_subscription(persist_state, prepared)
            })
            .unwrap_or_else(|response| response)
        })
        .await
    {
        Ok(response) => response,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            crate::http::store_wait::busy_response()
        }
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

fn prepare_subscription(
    state: &ApiState,
    user_id: UserId,
    body: CreateSubscriptionInput,
) -> Result<PreparedSubscription, Response> {
    let (media_input, kind_hint) = resolve_media_input(state, &body)?;
    let media_kind = kind_hint.ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "media.kind 必须是 movie 或 tv",
        )
    })?;
    let fetch_mode = FetchMode::from_str(&body.fetch_mode).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "fetch_mode 无效",
        )
    })?;
    let coverage = coverage_from_input(media_kind, body.coverage.clone(), body.full_season_pack)?;
    let mut incoming = incoming_media(media_kind, media_input);
    if incoming.tmdb_id.is_none() {
        enrich_tmdb_id_from_aliases(state, &mut incoming);
    }
    let explicit_library_id = body
        .library_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let auto_library = if explicit_library_id.is_none() {
        crate::http::routing_preview::auto_match_library(
            state,
            media_kind,
            incoming.tmdb_id.as_deref(),
            Some(user_id),
        )
    } else {
        None
    };
    Ok(PreparedSubscription {
        user_id,
        incoming,
        fetch_mode,
        coverage,
        explicit_library_id,
        auto_library,
        body,
    })
}

fn enrich_tmdb_id_from_aliases(state: &ApiState, incoming: &mut Media) {
    let aliases = [
        ("douban", incoming.douban_id.clone()),
        ("tvdb", incoming.tvdb_id.clone()),
        ("bangumi", incoming.bangumi_id.clone()),
        ("anilist", incoming.anilist_id.clone()),
    ];
    if !aliases.iter().any(|(_, id)| id.is_some()) {
        return;
    }
    let kind = incoming.kind;
    let lookup = crate::http::store_wait::with_store_read(state, &[store::Db::Library], |store| {
        for (source, maybe_id) in &aliases {
            let Some(id) = maybe_id.as_deref() else {
                continue;
            };
            match store.get_media_by_alias_kind(source, id, Some(kind)) {
                Ok(Some(stored)) if stored.tmdb_id.is_some() => return stored.tmdb_id,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, source, id, ?kind, "按外部别名回填 tmdb_id 查询失败");
                }
            }
        }
        None
    });
    match lookup {
        Ok(Some(tmdb_id)) => incoming.tmdb_id = Some(tmdb_id),
        Ok(None) => {}
        Err(_) => {
            tracing::warn!(?kind, "按外部别名回填 tmdb_id 获取读快照失败");
        }
    }
}

fn resolve_media_input(
    state: &ApiState,
    body: &CreateSubscriptionInput,
) -> Result<(MediaInput, Option<MediaKind>), Response> {
    let (mut media_input, kind_hint) = if let Some(raw) = &body.title_ref {
        media_from_title_ref(state, raw)?
    } else if let Some(media) = body.media.clone() {
        let kind_hint = MediaKind::from_str(&media.kind).ok();
        (media, kind_hint)
    } else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "media 或 title_ref 必填",
        ));
    };
    if let Some(raw_source) = &body.source_title_ref
        && let Ok(crate::http::title_ref::TitleRef::Douban(douban_id)) =
            crate::http::title_ref::parse_title_ref(raw_source)
        && media_input.douban_id.is_none()
    {
        media_input.douban_id = Some(douban_id);
    }
    Ok((media_input, kind_hint))
}

fn media_from_title_ref(
    state: &ApiState,
    raw: &str,
) -> Result<(MediaInput, Option<MediaKind>), Response> {
    let title = crate::http::title_ref::parse_title_ref(raw)
        .map_err(|reason| crate::http::title_ref::invalid_title_ref(&reason))?;
    match crate::http::title_ref::resolve_title(state, &title) {
        Ok(Some(media)) => {
            let kind = media.kind;
            let identity = crate::http::title_ref::media_identity(&media);
            let identity: MediaInput = serde_json::from_value(identity).map_err(|_| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    "media identity 序列化失败",
                )
            })?;
            Ok((identity, Some(kind)))
        }
        Ok(None) => Err(err(
            StatusCode::NOT_FOUND,
            "media.missing",
            "title_ref 无法解析到条目",
        )),
        Err(error) => Err(err(StatusCode::BAD_GATEWAY, "media.upstream", &error)),
    }
}

fn coverage_from_input(
    media_kind: MediaKind,
    coverage: Option<CoverageInput>,
    full_season_pack: bool,
) -> Result<Coverage, Response> {
    let coverage = match coverage {
        Some(CoverageInput::Movie) => Coverage::Movie,
        Some(CoverageInput::Tv {
            season,
            episode_from,
            episode_to,
        }) => Coverage::Tv {
            season,
            episode_from,
            episode_to,
        },
        None => {
            let default = crate::http::title_ref::default_coverage(media_kind);
            if matches!(media_kind, MediaKind::Movie) != matches!(default, Coverage::Movie) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "subscription.invalid",
                    "media.kind 与 coverage 不一致",
                ));
            }
            default
        }
    };
    if matches!(media_kind, MediaKind::Movie) != matches!(coverage, Coverage::Movie) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "media.kind 与 coverage 不一致",
        ));
    }
    coverage
        .validate(full_season_pack)
        .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))?;
    Ok(coverage)
}

fn persist_subscription(state: ApiState, prepared: PreparedSubscription) -> Response {
    let auto_next_season = prepared.body.auto_next_season;
    let pack_fallback_after_days = prepared.body.pack_fallback_after_days;
    let subscribe = match subscribe_from_prepared(&state, &prepared) {
        Ok(subscribe) => subscribe,
        Err(response) => return response,
    };
    let (media, subscribe) = match create_with_auto_library_fallback(&state, &prepared, subscribe) {
        Ok(result) => result,
        Err(response) => return response,
    };
    if let Some(days) = pack_fallback_after_days {
        crate::pack_fallback::set_after_days(&state, subscribe.id, Some(days));
    }
    if auto_next_season {
        crate::completion::set_auto_next_season(&state, subscribe.id, true);
    }
    let snapshot = {
        let store = state.store.as_ref();
        let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
        super::snapshot::load_single_subscription_view(&store, &subscribe, &media, &facts)
    };
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(%error, "Subscribe timestamp read failed");
            return store_err_response(error);
        }
    };
    let body = super::views::render_subscription_snapshot(&snapshot, state.file_stats.as_ref());
    (StatusCode::CREATED, ok(body)).into_response()
}

fn create_with_auto_library_fallback(
    state: &ApiState,
    prepared: &PreparedSubscription,
    mut subscribe: Subscribe,
) -> Result<(Media, Subscribe), Response> {
    let first =
        crate::subscribe_create::create(state, prepared.incoming.clone(), subscribe.clone(), None);
    let outcome = match first {
        Err(crate::subscribe_create::CreateError::Store(store::StoreError::Conflict(_)))
            if prepared.explicit_library_id.is_none()
                && match subscribe.library_id {
                    Some(id) => !state
                        .store
                        .library_accepts_work(&id.to_string())
                        .map_err(store_err_response)?,
                    None => false,
                } =>
        {
            tracing::warn!(
                library_id = ?subscribe.library_id,
                "自动匹配的目标媒体库在创建订阅期间被删除或隔离，回退默认媒体库重试"
            );
            reset_subscribe_to_default_library(
                &state.store,
                &mut subscribe,
                &prepared.body,
                prepared.incoming.kind,
                prepared.user_id,
            )?;
            crate::subscribe_create::create(state, prepared.incoming.clone(), subscribe, None)
        }
        other => other,
    };
    outcome.map_err(|error| map_create_error(&state.store, prepared.user_id, error))
}

fn reset_subscribe_to_default_library(
    store: &crate::Store,
    subscribe: &mut Subscribe,
    body: &CreateSubscriptionInput,
    media_kind: MediaKind,
    user_id: UserId,
) -> Result<(), Response> {
    let is_admin = store.user_role(user_id).map_err(store_err_response)? == "admin";
    let fallback_library_id = resolve_untargeted_library(store, media_kind, user_id, is_admin)?;
    subscribe.library_id = fallback_library_id;
    if body.filter_id.is_none() {
        let effective_lib = fallback_library_id.map(|id| id.to_string());
        let filter_id = resolve_filter_id(
            store,
            body,
            effective_lib.as_deref(),
            media_kind,
            user_id,
            is_admin,
        )?;
        subscribe.filter_id = filter_id;
        if body.keep_old_versions.is_none() {
            subscribe.keep_old_versions = store
                .get_filter(filter_id)
                .map_err(store_err_response)?
                .map(|f| f.keep_old_versions)
                .unwrap_or(false);
        }
    }
    Ok(())
}

fn resolve_untargeted_library(
    store: &crate::Store,
    media_kind: MediaKind,
    user_id: UserId,
    is_admin: bool,
) -> Result<Option<domain::LibraryId>, Response> {
    let ownership = store.library_ownership().map_err(store_err_response)?;
    let kind_libs: Vec<&store::Library> = ownership
        .libraries()
        .iter()
        .filter(|lib| lib.kind == media_kind)
        .collect();
    if kind_libs.is_empty() {
        return Ok(None);
    }
    let can_auto_route = |lib: &&store::Library| {
        ownership.accepts_work(&lib.id)
            && crate::http::routing_preview::user_can_auto_route_library(
                lib,
                Some(user_id),
                is_admin,
            )
    };
    if kind_libs
        .iter()
        .any(|lib| lib.is_default && can_auto_route(lib))
    {
        return Ok(None);
    }
    if let Some(fallback) = kind_libs
        .iter()
        .find(|lib| lib.match_rules.is_empty() && can_auto_route(lib))
    {
        let id = domain::LibraryId::from_str(&fallback.id).map_err(|_| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "媒体库 ID 格式无效",
            )
        })?;
        return Ok(Some(id));
    }
    Err(err(
        StatusCode::BAD_REQUEST,
        "subscription.invalid",
        "没有可用的目标媒体库",
    ))
}

pub(super) fn overlap_conflict_response(
    store: &crate::Store,
    user_id: UserId,
    other_id: &str,
) -> Response {
    let can_disclose = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false)
        || SubscribeId::from_str(other_id)
            .ok()
            .and_then(|sid| store.get_subscribe(sid).ok().flatten())
            .is_some_and(|existing| existing.user_id == user_id);
    let message = if can_disclose {
        format!("同一媒体库中已有覆盖重叠的订阅（{other_id}），请合并或调整集数范围")
    } else {
        "同一媒体库中已有覆盖重叠的订阅，请调整集数范围".to_string()
    };
    err(StatusCode::CONFLICT, "subscription.overlap", &message)
}

fn map_create_error(
    store: &crate::Store,
    user_id: UserId,
    error: crate::subscribe_create::CreateError,
) -> Response {
    match error {
        crate::subscribe_create::CreateError::Overlap(other) => {
            overlap_conflict_response(store, user_id, &other.to_string())
        }
        crate::subscribe_create::CreateError::Store(
            store::StoreError::ReadBusy
            | store::StoreError::ReadClosed
            | store::StoreError::ReadDeadline
            | store::StoreError::ReadCancelled
            | store::StoreError::WriteBusy,
        ) => crate::http::store_wait::busy_response(),
        crate::subscribe_create::CreateError::Store(store::StoreError::Conflict(message)) => {
            err(StatusCode::CONFLICT, "subscription.conflict", &message)
        }
        crate::subscribe_create::CreateError::Admission(
            crate::jobs_writer::JobsMutationError::Acquire(error),
        ) => crate::http::jobs::jobs_acquire_response(error),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subscription.create_failed",
            &error.to_string(),
        ),
    }
}

fn subscribe_from_prepared(
    state: &ApiState,
    prepared: &PreparedSubscription,
) -> Result<Subscribe, Response> {
    let media_kind = prepared.incoming.kind;
    let user_id = prepared.user_id;
    let body = &prepared.body;
    let store = state.store.as_ref();
    let is_admin = store.user_role(user_id).map_err(store_err_response)? == "admin";
    let library_id = match prepared.explicit_library_id.as_deref() {
        Some(raw) => super::library_target::resolve(store, Some(raw), media_kind, user_id)?,
        None => {
            let live_auto_lib = match prepared.auto_library.as_ref() {
                Some(lib) => store.get_library(&lib.id).map_err(store_err_response)?,
                None => None,
            };
            let live_auto_accepts = match live_auto_lib.as_ref() {
                Some(lib) => store
                    .library_accepts_work(&lib.id)
                    .map_err(store_err_response)?,
                None => false,
            };
            match live_auto_lib
                .as_ref()
                .filter(|lib| {
                    lib.kind == media_kind
                        && live_auto_accepts
                        && crate::http::routing_preview::user_can_auto_route_library(
                            lib,
                            Some(user_id),
                            is_admin,
                        )
                })
                .and_then(|lib| domain::LibraryId::from_str(&lib.id).ok())
            {
                Some(matched_id) => Some(matched_id),
                None => resolve_untargeted_library(store, media_kind, user_id, is_admin)?,
            }
        }
    };
    let effective_library_id = library_id.map(|id| id.to_string());
    let filter_id = resolve_filter_id(
        store,
        body,
        effective_library_id.as_deref(),
        media_kind,
        user_id,
        is_admin,
    )?;
    let filter_keep_old = store
        .get_filter(filter_id)
        .map_err(store_err_response)?
        .map(|f| f.keep_old_versions)
        .unwrap_or(false);
    let effective_keep_old = body.keep_old_versions.unwrap_or(filter_keep_old);
    Ok(Subscribe {
        id: SubscribeId::new(),
        user_id,
        media_id: prepared.incoming.id,
        coverage: prepared.coverage.clone(),
        fetch_mode: prepared.fetch_mode,
        filter_id,
        wash_cut: body.wash_cut,
        keep_old_versions: effective_keep_old,
        wash_cut_filter_id: optional_filter_id(store, body.wash_cut_filter_id.as_deref())?,
        full_season_pack: body.full_season_pack,
        downloader_id: optional_downloader_id(store, body.downloader_id.as_deref())?,
        library_id,
        tracking_state: "active".into(),
        follow_future: body.follow_future,
        search_interval_secs: match body.search_interval_secs {
            Some(secs) => crate::http::subscribe_schedule::clamp_interval(secs)
                .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))?,
            None => crate::http::subscribe_schedule::default_interval(),
        },
    })
}

fn incoming_media(media_kind: MediaKind, media_input: MediaInput) -> Media {
    Media {
        id: MediaId::new(),
        kind: media_kind,
        title: media_input.title,
        year: media_input.year,
        original_title: media_input.original_title.filter(|t| !t.is_empty()),
        aliases: media_input.aliases,
        tmdb_id: media_input.tmdb_id.filter(|id| !id.is_empty()),
        douban_id: media_input.douban_id.filter(|id| !id.is_empty()),
        tvdb_id: media_input.tvdb_id.filter(|id| !id.is_empty()),
        bangumi_id: media_input.bangumi_id.filter(|id| !id.is_empty()),
        anilist_id: media_input.anilist_id.filter(|id| !id.is_empty()),
    }
}

fn resolve_filter_id(
    store: &crate::Store,
    body: &CreateSubscriptionInput,
    library_id: Option<&str>,
    media_kind: MediaKind,
    user_id: UserId,
    is_admin: bool,
) -> Result<FilterId, Response> {
    match &body.filter_id {
        Some(raw) => existing_filter_id(store, raw),
        None => inherited_filter_id(store, library_id, media_kind, user_id, is_admin),
    }
}

fn inherited_filter_id(
    store: &crate::Store,
    library_id: Option<&str>,
    media_kind: MediaKind,
    user_id: UserId,
    is_admin: bool,
) -> Result<FilterId, Response> {
    let target_library = match library_id.filter(|id| !id.is_empty()) {
        Some(id) => store.get_library(id).map_err(store_err_response)?,
        None => match store
            .default_library(media_kind)
            .map_err(store_err_response)?
        {
            Some(lib)
                if store
                    .library_accepts_work(&lib.id)
                    .map_err(store_err_response)?
                    && crate::http::routing_preview::user_can_auto_route_library(
                        &lib,
                        Some(user_id),
                        is_admin,
                    ) =>
            {
                Some(lib)
            }
            _ => None,
        },
    };
    if let Some(lib) = &target_library
        && let Some(raw) = lib.default_filter_id.as_deref().filter(|s| !s.is_empty())
    {
        match FilterId::from_str(raw) {
            Ok(id) => match store.get_filter(id).map_err(store_err_response)? {
                Some(_) => return Ok(id),
                None => {
                    tracing::warn!(
                        library_id = %lib.id,
                        default_filter_id = %raw,
                        "媒体库默认规则组不存在，回退全局默认规则组"
                    );
                }
            },
            Err(_) => {
                tracing::warn!(
                    library_id = %lib.id,
                    default_filter_id = %raw,
                    "媒体库默认规则组 ID 格式无效，回退全局默认规则组"
                );
            }
        }
    }
    let id = match crate::management::default_filter_id(store) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "未配置默认 Filter，请提供 filter_id",
            ));
        }
        Err(error) => return Err(error.into_response()),
    };
    existing_filter(store, id, "默认 Filter 不存在，请提供 filter_id")
}

fn existing_filter_id(store: &crate::Store, raw: &str) -> Result<FilterId, Response> {
    let id = FilterId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "filter_id 无效",
        )
    })?;
    existing_filter(store, id, "filter_id 无效")
}

fn existing_filter(
    store: &crate::Store,
    id: FilterId,
    missing: &str,
) -> Result<FilterId, Response> {
    match store.get_filter(id) {
        Ok(Some(_)) => Ok(id),
        Ok(None) => Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            missing,
        )),
        Err(error) => Err(store_err_response(error)),
    }
}

fn optional_filter_id(
    store: &crate::Store,
    raw: Option<&str>,
) -> Result<Option<FilterId>, Response> {
    let Some(raw) = raw.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let id = FilterId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "wash_cut_filter_id 无效",
        )
    })?;
    existing_filter(store, id, "wash_cut_filter_id 无效").map(Some)
}

fn optional_downloader_id(
    store: &crate::Store,
    raw: Option<&str>,
) -> Result<Option<domain::DownloaderId>, Response> {
    let Some(raw) = raw.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let id = domain::DownloaderId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "downloader_id 无效",
        )
    })?;
    match store.get_downloader(id) {
        Ok(Some(_)) => Ok(Some(id)),
        Ok(None) => Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "downloader_id 无效",
        )),
        Err(error) => Err(store_err_response(error)),
    }
}

fn store_err_response(error: store::StoreError) -> Response {
    match error {
        store::StoreError::ReadBusy
        | store::StoreError::ReadClosed
        | store::StoreError::ReadDeadline
        | store::StoreError::ReadCancelled
        | store::StoreError::WriteBusy => crate::http::store_wait::busy_response(),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct NoFetch;
    impl indexer::Fetcher for NoFetch {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("offline".into()))
        }
    }

    #[test]
    fn subscribe_from_prepared_propagates_library_accepts_work_and_busy_errors() {
        for err_variant in [
            store::StoreError::ReadBusy,
            store::StoreError::ReadDeadline,
            store::StoreError::WriteBusy,
        ] {
            let resp = store_err_response(err_variant);
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        }

        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(tmp.path().join("data")).unwrap());
        let state = ApiState::new_arc(
            store.clone(),
            "management-secret".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let admin_id = store
            .user_id_by_token("management-secret")
            .unwrap()
            .unwrap();
        let root_b = tmp.path().join("tv-auto");
        std::fs::create_dir_all(&root_b).unwrap();
        let auto_lib = store
            .create_library_with_rules(
                MediaKind::Tv,
                "CN",
                &[root_b.to_str().unwrap()],
                "everyone",
                true,
                &[],
                &[serde_json::json!({"field":"origin_countries","op":"any_of","values":["CN"]})],
            )
            .unwrap();
        let media_in: MediaInput = serde_json::from_value(
            serde_json::json!({"kind":"tv","title":"Test","tmdb_id":"203483"}),
        )
        .unwrap();
        let prepared = PreparedSubscription {
            user_id: admin_id,
            incoming: incoming_media(MediaKind::Tv, media_in),
            fetch_mode: FetchMode::Search,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: None,
            },
            explicit_library_id: None,
            auto_library: Some(auto_lib.clone()),
            body: serde_json::from_value(serde_json::json!({"fetch_mode":"search"})).unwrap(),
        };

        // `get_library` succeeds while `library_accepts_work` fails querying `library_removals`:
        // error must propagate right at `library_accepts_work` via `store_err_response`.
        let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
        conn.execute_batch("DROP TABLE library_removals;").unwrap();
        let Err(sql_err) = subscribe_from_prepared(&state, &prepared) else {
            panic!("library_accepts_work error must propagate");
        };
        assert_eq!(sql_err.status(), StatusCode::INTERNAL_SERVER_ERROR);

        store.reader_pool(store::Db::Library).close();
        let Err(busy_err) = subscribe_from_prepared(&state, &prepared) else {
            panic!("closed reader pool must return 503");
        };
        assert_eq!(busy_err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
