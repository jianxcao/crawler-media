use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

mod delivery;
use domain::SubscribeId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::management::{ApiError, ApiState};

#[derive(Deserialize)]
pub struct AdmitBody {
    subscribe_id: String,
    enclosure: String,
    #[serde(default)]
    release: Option<ReleaseOverride>,
}

#[derive(Deserialize)]
pub struct ReleaseOverride {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<u16>,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    codec: Option<String>,
    #[serde(default)]
    hdr: Option<String>,
}

fn apply_admit_release_override(
    base: domain::Release,
    override_rel: &ReleaseOverride,
) -> domain::Release {
    domain::Release {
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
) -> Result<
    (
        domain::Subscribe,
        domain::Media,
        domain::Filter,
        Vec<domain::Site>,
    ),
    ApiError,
> {
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
    Json(body): Json<AdmitBody>,
) -> Result<Json<Value>, Response> {
    let budget = state.http_work.clone();
    budget
        .run_to_completion(move || admit_blocking(state, body))
        .await
        .map_err(|error| match error {
            crate::blocking_budget::BlockingError::Busy => crate::http::store_wait::busy_response(),
            error => {
                tracing::error!(%error, "Manual Torrent admission worker failed");
                ApiError::internal(error.to_string()).into_response()
            }
        })?
}

fn admit_blocking(state: ApiState, body: AdmitBody) -> Result<Json<Value>, Response> {
    let subscribe_id = body.subscribe_id.parse::<SubscribeId>().map_err(|_| {
        ApiError::invalid("subscribe.invalid", "invalid Subscribe id".into()).into_response()
    })?;
    let guard = state.subscribe_guard(subscribe_id);
    let Some(_lease) = guard.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT) else {
        return Err(crate::http::store_wait::busy_response());
    };
    if state.subscribe_is_deleting(subscribe_id) {
        return Err(ApiError::with(
            axum::http::StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除".into(),
        )
        .into_response());
    }
    let (subscribe, media, filter, sites) =
        admitted_store(&state, |store| load_admit_context(store, subscribe_id))?;
    let keyword = media
        .original_title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&media.title);
    let torrents = state.indexer.search(&sites, keyword).torrents;
    let Some(chosen) = torrents
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
    let admitted = filter::admit_scored(vec![(chosen.clone(), Some(corrected))], &filter).admitted;
    let Some(scored) = admitted.into_iter().next() else {
        return Err(
            ApiError::invalid("subscribe.rejected", "Torrent rejected by Filter".into())
                .into_response(),
        );
    };
    let delivery = delivery::prepare(&state, &subscribe, &scored.torrent)?;
    let url = delivery.resolve_url(&state, &scored.torrent)?;
    let pending = crate::store::PendingDownload {
        // Existing pending representation: zero means the remote outcome is unconfirmed.
        submitted_at: Some(0),
        torrent: scored.torrent.clone(),
        release_override: override_for_pending,
        downloader_id: delivery.id,
    };
    admitted_store(&state, |store| {
        store
            .record_manual_pending_submissions(subscribe.id, &[(scored.score, pending)])
            .map_err(ApiError::from)
    })?;
    delivery.submit(&scored.torrent, &url)?;
    // This closes an already admitted side effect. A new maintenance/readonly gate
    // must not discard its result; the persisted intent survives any confirmation failure.
    if let Err(error) = state.store.confirm_pending_submission(
        subscribe.id,
        &scored.torrent.enclosure,
        crate::job_loop::unix_now(),
    ) {
        tracing::error!(%error, subscribe_id=%subscribe.id, "Submitted manual Torrent retains unconfirmed pending intent");
        return Err(ApiError::from(error).into_response());
    }
    tracing::info!(subscribe_id=%subscribe.id, title=%scored.torrent.title, score=scored.score, "Manual Torrent admitted and submitted");
    Ok(Json(json!({
        "subscribe_id": subscribe.id.to_string(),
        "title": scored.torrent.title,
        "enclosure": scored.torrent.enclosure,
        "score": scored.score,
    })))
}

pub async fn admit_torrent_v1(
    state: State<ApiState>,
    body: Json<AdmitBody>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match admit_torrent(state, body).await {
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
