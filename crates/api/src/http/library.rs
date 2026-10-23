//! Library browse: Library listing and item views rendered from the `library_items`
//! summaries. Entity CRUD lives in `library_config`, scans/refresh in `library_scan`,
//! artwork in `library_artwork`, admin views in `library_admin`.

mod probe;
pub(crate) use probe::{
    enqueue_probes_for_files, enqueue_probes_for_paths, ensure_probe_enqueued, probe_item,
};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{MediaId, MediaKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::store::{Library, library_query};

mod assets;
mod cover;
pub(crate) mod page;
pub(crate) use assets::{
    coverages_in_library, files_in_library, library_json, poster_path, poster_path_with,
    preferred_coverage,
};
pub(crate) use cover::{default_cover_options, render_cover_jpeg};

mod query;
mod visibility;
pub(crate) use query::{
    BROWSE_DEADLINE, browse_filter_from_query, browse_order_from_query, browse_store_error,
    browse_work_error,
};
pub(crate) use visibility::{
    artwork_url, backdrop_path, backdrop_path_with, clear_library_cover_checked,
    file_visible_to_user, library_cover_path, library_for_file, library_for_file_strict,
    library_visible, library_visible_with_role, missing_library, require_visible_library,
};

/// GET /libraries?kind= — one entry per Library entity, defaults first;
/// members only see libraries visible to them.
pub(crate) async fn list_libraries(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // The filtered library roster is captured once and shared by statistics and rendering.
    // 锁内只收集 owned DB/路径事实（removal / 封面候选路径）；逐 root `is_dir`、
    // 封面 `is_file` 仍在锁外、受 http_work 预算约束，慢挂载不再阻塞共享 Store
    // 和 Tokio worker。
    let (facts, roster, is_admin) = {
        let store = state.store.lock();
        let libraries = match store.list_libraries() {
            Ok(libraries) => libraries,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let is_admin = user_id
            .and_then(|uid| store.user_role(uid).ok())
            .as_deref()
            .map(|r| r == "admin")
            .unwrap_or(false);
        let manage_scope = query.get("scope").is_some_and(|s| s == "admin")
            || query.get("manage").is_some_and(|s| s == "1" || s == "true");
        let kind = query.get("kind").and_then(|k| MediaKind::from_str(k).ok());
        let roster: Vec<Library> = libraries
            .into_iter()
            .filter(|library| {
                (is_admin && manage_scope)
                    || library_visible_with_role(&store, library, user_id, is_admin)
            })
            .filter(|library| kind.is_none_or(|k| library.kind == k))
            .collect();
        let facts: Vec<assets::LibraryFacts> = roster
            .iter()
            .map(|library| assets::library_facts(&store, library))
            .collect();
        (facts, roster, is_admin)
    };
    let stats = match crate::library_statistics::collect_statistics_for_roster(
        state.clone(),
        user_id,
        &roster,
        is_admin,
    )
    .await
    {
        Ok(stats) => stats,
        Err(res) => return res,
    };
    match state
        .http_work
        .run(BROWSE_DEADLINE, move || {
            facts
                .iter()
                .map(|facts| {
                    let stats = stats.get(&facts.library.id).copied();
                    assets::library_json_from_facts(facts, stats)
                })
                .collect::<Vec<Value>>()
        })
        .await
    {
        Ok(items) => ok_list(items).into_response(),
        Err(error) => {
            tracing::error!(%error, "媒体库列表的文件观察任务失败");
            browse_work_error(error)
        }
    }
}

/// GET /libraries/{id}/items — one card per Media owned under this library's roots.
///
/// Filters, ordering and paging are all SQL over `library_items`; the card extras (seasons,
/// studio) are read once for the page's media ids, so a page costs a fixed number of queries
/// and never one per card.
pub(crate) async fn list_items(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
    }
    let filter = match browse_filter_from_query(&query) {
        Ok(filter) => filter,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let (order, descending) = browse_order_from_query(&query);
    // Offset remains available for index jumps, while every response is bounded.
    let window = match crate::http::library::page::PageWindow::parse(&query) {
        Ok(window) => window,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let limit = window.limit;
    let lib_q = library_query::LibraryQuery {
        library_id: lib_id,
        user_id,
        filter,
        order,
        descending,
        page: library_query::BrowsePage::Offset {
            start: window.offset,
            limit,
        },
        include_total: true,
    };
    let (page, facts) = {
        let store = state.store.lock();
        let page = match store.library_item_page(&lib_q) {
            Ok(page) => page,
            Err(error) => return browse_store_error(error),
        };
        let media_ids: Vec<MediaId> = page.items.iter().map(|item| item.media_id).collect();
        match store.library_item_facts(lib_id, &media_ids, user_id) {
            Ok(facts) => (page, facts),
            Err(error) => return browse_store_error(error),
        }
    };
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|item| {
            let facts = facts.get(&item.media_id).cloned().unwrap_or_default();
            // The artwork routes (`/posters/{file_id}`, `/fanart/{file_id}`) serve the image
            // beside the file and answer 404 when it is absent. Cards historically always
            // carried a URL built from the preferred file; keep that contract when no file
            // recorded artwork, so a card never loses the image it had before.
            let poster_ref = item.poster_ref.or(item.preferred_file_id);
            let backdrop_ref = item.backdrop_ref.or(item.preferred_file_id);
            json!({
                "media_item_id": item.media_id.to_string(),
                "kind": item.kind.as_str(),
                "library_id": item.library_id.to_string(),
                "title": item.title,
                "year": item.year,
                "rating": item.rating,
                "studio": facts.studio,
                "studio_logo": facts
                    .studio_logo
                    .map(|path| format!("https://image.tmdb.org/t/p/w92{path}")),
                "poster_url": poster_ref.map(|file_id| artwork_url("posters", file_id)),
                "backdrop_url": backdrop_ref.map(|file_id| artwork_url("fanart", file_id)),
                "file_count": item.file_count,
                "seasons": facts.seasons,
                "episode_count": item.distinct_episode_count,
                "max_resolution": item.max_resolution(),
                "missing": item.missing,
                "has_poster": item.has_poster(),
                "has_backdrop": item.has_backdrop(),
            })
        })
        .collect();
    ok_list(items).into_response()
}

/// GET /libraries/{id}/items/{item_id} — adopted metadata plus the file list. Nothing here
/// observes a file: art and metadata are stored facts.
pub(crate) async fn item_detail(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    let (detail, media, details) = {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
        let detail = match store.library_item_detail(lib_id, media_id, "") {
            Ok(Some(detail)) => detail,
            Ok(None) => return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"),
            Err(error) => return browse_store_error(error),
        };
        let media = match store.get_media(media_id) {
            Ok(media) => media,
            Err(error) => return browse_store_error(error),
        };
        let file_ids: Vec<domain::LedgerId> =
            detail.files.iter().map(|file| file.file_id).collect();
        let details = match store.file_details_batch(&file_ids) {
            Ok(details) => details,
            Err(error) => return browse_store_error(error),
        };
        (detail, media, details)
    };
    let mut files = Vec::with_capacity(detail.files.len());
    for file in &detail.files {
        let units = detail
            .file_units
            .iter()
            .find(|(file_id, _)| *file_id == file.file_id)
            .map(|(_, units)| units.as_slice())
            .unwrap_or_default();
        let tracks = match details.get(&file.file_id) {
            Some(details) => match tracks_from_details(details) {
                Ok(tracks) => tracks,
                Err(error) => return browse_store_error(error),
            },
            None => library::Tracks::default(),
        };
        files.push(item_file_json(&state, file, units, &tracks));
    }
    let metadata = &detail.metadata;
    let cast: Vec<Value> = metadata
        .cast
        .value()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|member| {
            let avatar_url = member.thumb.as_ref().map(|thumb| {
                if thumb.starts_with("http://") || thumb.starts_with("https://") {
                    thumb.clone()
                } else {
                    format!("https://image.tmdb.org/t/p/w185{thumb}")
                }
            });
            json!({
                "name": member.name,
                "role": member.role,
                "tmdb_person_id": member.tmdb_id.as_deref().and_then(|id| id.parse::<i64>().ok()),
                "avatar_url": avatar_url,
                "order": member.order,
            })
        })
        .collect();
    let item = &detail.item;
    let (original_title, tmdb_id) = match media {
        Some(media) => (media.original_title.clone(), media.tmdb_id.clone()),
        None => (None, None),
    };
    ok(json!({
        "media_item_id": item.media_id.to_string(),
        "kind": item.kind.as_str(),
        "title": item.title,
        "year": item.year,
        "original_title": metadata.original_title.value().cloned().or(original_title),
        "tmdb_id": tmdb_id,
        "overview": metadata.overview.value(),
        "rating": metadata.rating.value().map(f64::to_string),
        "vote_count": metadata.vote_count.value(),
        "runtime_minutes": metadata.runtime_minutes.value().map(u32::to_string),
        "tagline": metadata.tagline.value(),
        "premiered": metadata.release_date.value(),
        "end_date": metadata.end_date.value(),
        "content_rating": metadata.content_rating.value(),
        "original_language": metadata.language.value(),
        "status": metadata.status.value(),
        "number_of_seasons": metadata.number_of_seasons.value(),
        "number_of_episodes": metadata.number_of_episodes.value(),
        "genres": metadata
            .genres
            .value()
            .map(|genres| genres.iter().map(|genre| genre.name.clone()).collect::<Vec<_>>())
            .unwrap_or_default(),
        "countries": metadata.countries.value().cloned().unwrap_or_default(),
        "studios": metadata.studios.value().cloned().unwrap_or_default(),
        "directors": metadata.directors.value().cloned().unwrap_or_default(),
        "creators": metadata.creators.value().cloned().unwrap_or_default(),
        "cast": cast,
        "poster_url": item.poster_ref.map(|file_id| artwork_url("posters", file_id)),
        "backdrop_url": item.backdrop_ref.map(|file_id| artwork_url("fanart", file_id)),
        "files": files,
        "next_file_cursor": detail.next_file_cursor,
        "seasons": detail.seasons,
    }))
    .into_response()
}

/// GET /libraries/{id}/items/{item_id}/files?after=&limit= — one bounded page of files.
pub(crate) async fn item_files(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Ok(lib_id) = id.parse::<domain::LibraryId>() else {
        return missing_library();
    };
    let Ok(media_id) = MediaId::from_str(&item_id) else {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效");
    };
    let limit = match query
        .get("limit")
        .map(String::as_str)
        .unwrap_or("50")
        .parse::<usize>()
    {
        Ok(limit @ 1..=100) => limit,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid",
                "limit 必须在 1..=100 之间",
            );
        }
    };
    let after = query.get("after").map(String::as_str);
    let page = {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
        match store.library_file_page(lib_id, media_id, after, limit) {
            Ok(page) => page,
            Err(error) => return browse_store_error(error),
        }
    };
    let file_ids: Vec<_> = page.files.iter().map(|file| file.file_id).collect();
    let (file_units, details) = {
        let store = state.store.lock();
        let units = match store.file_units(lib_id, media_id, &page.files) {
            Ok(units) => units,
            Err(error) => return browse_store_error(error),
        };
        let details = match store.file_details_batch(&file_ids) {
            Ok(details) => details,
            Err(error) => return browse_store_error(error),
        };
        (units, details)
    };
    let mut files = Vec::with_capacity(page.files.len());
    for file in &page.files {
        let units = file_units
            .iter()
            .find(|(id, _)| *id == file.file_id)
            .map(|(_, units)| units.as_slice())
            .unwrap_or_default();
        let tracks = match details.get(&file.file_id) {
            Some(details) => match tracks_from_details(details) {
                Ok(tracks) => tracks,
                Err(error) => return browse_store_error(error),
            },
            None => library::Tracks::default(),
        };
        files.push(item_file_json(&state, file, units, &tracks));
    }
    ok(json!({ "files": files, "next_cursor": page.next_cursor })).into_response()
}

/// One file of the detail view: stored quality facts plus whatever the probe has cached.
fn item_file_json(
    state: &ApiState,
    file: &domain::LibraryFile,
    units: &[domain::UnitKey],
    tracks: &library::Tracks,
) -> Value {
    let unit = units
        .iter()
        .find(|unit| !unit.is_whole())
        .or_else(|| units.first());

    json!({
        "file_id": file.file_id.to_string(),
        "path": file.path.to_string_lossy(),
        // Complete coverage: one physical file may serve several episodes, so the
        // full unit list travels with it. `season`/`episode` stay as the first
        // concrete coordinate for consumers that only pick a default version.
        "units": units
            .iter()
            .filter(|unit| !unit.is_whole())
            .map(|unit| json!({ "season": unit.season, "episode": unit.episode }))
            .collect::<Vec<_>>(),
        "presence": file.presence.as_str(),
        "publication": file.publication.as_str(),
        "season": unit.map(|unit| unit.season).filter(|season| *season >= 0),
        "episode": unit.map(|unit| unit.episode).filter(|episode| *episode >= 0),
        "resolution": file.resolution,
        "codec": file.codec,
        "hdr": file.hdr,
        "quality_source": file.quality_source.as_str(),
        "probed": tracks.video.is_some()
            || !tracks.audio.is_empty()
            || !tracks.subtitles.is_empty(),
        "probe_queued": state.probe.is_queued(&file.file_id.to_string()),
        "video_track": tracks.video.as_ref().map(|v| json!({
            "codec": v.codec, "width": v.width, "height": v.height,
            "frame_rate": v.frame_rate, "bit_rate": v.bit_rate,
            "duration_secs": v.duration_secs,
        })),
        "audio_tracks": tracks.audio.iter().map(|t| json!({
            "codec": t.codec, "profile": null, "channels": t.channels,
            "channel_layout": null, "language": t.language,
            "sample_rate": t.sample_rate, "title": null,
            "default": t.is_default,
        })).collect::<Vec<_>>(),
        "subtitle_tracks": tracks.subtitles.iter().map(|t| json!({
            "codec": t.codec, "language": t.language, "title": null,
            "forced": t.forced, "default": t.is_default,
            "external": t.is_external, "file_name": null,
        })).collect::<Vec<_>>(),
    })
}

/// Tracks of one file from the probe cache. Malformed cache JSON is an error, never a silent
/// empty track list: the detail page must not claim "not probed" for a corrupt row.
fn tracks_from_details(
    details: &store::files::FileDetails,
) -> Result<library::Tracks, store::StoreError> {
    let video =
        match details.video_json.as_deref() {
            Some(raw) => Some(serde_json::from_str::<library::VideoTrack>(raw).map_err(
                |error| store::StoreError::Invalid(format!("file details video: {error}")),
            )?),
            None => None,
        };
    let audio = serde_json::from_str::<Vec<library::AudioTrack>>(&details.audio_json)
        .map_err(|error| store::StoreError::Invalid(format!("file details audio: {error}")))?;
    let subtitles = serde_json::from_str::<Vec<library::SubtitleTrack>>(&details.subtitle_json)
        .map_err(|error| store::StoreError::Invalid(format!("file details subtitles: {error}")))?;
    Ok(library::Tracks {
        video,
        audio,
        subtitles,
    })
}

/// GET /libraries/{id}/items/{item_id}/episodes?season= — the episode list from adopted
/// catalog rows plus ownership from the published file facts, for the current viewer.
pub(crate) async fn item_episodes(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let season = match query.get("season") {
        Some(raw) => match raw.parse::<u32>() {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::error!(library_id = %id, media_id = %media_id, season = %raw, error = %error, "媒体库分集请求季号无效");
                return err(StatusCode::BAD_REQUEST, "library.invalid", "季号参数无效");
            }
        },
        None => None,
    };
    let lib_id = match id.parse::<domain::LibraryId>() {
        Ok(lib_id) => lib_id,
        Err(_) => return missing_library(),
    };
    let episodes = {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
        match store.get_library_item(lib_id, media_id) {
            Ok(Some(_)) => {}
            Ok(None) => return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"),
            Err(error) => return browse_store_error(error),
        }
        match store.library_episodes(lib_id, media_id, season, "") {
            Ok(episodes) => episodes,
            Err(error) => return browse_store_error(error),
        }
    };
    let mut effective_states = HashMap::new();
    if let Some(user_id) = user_id {
        let store = state.store.lock();
        for episode in &episodes {
            let unit_state = match crate::http::playback::effective_watch_state(
                &store,
                user_id,
                media_id,
                episode.season as i32,
                episode.episode as i32,
            ) {
                Ok(state) => state,
                Err(error) => return browse_store_error(error),
            };
            effective_states.insert((episode.season, episode.episode), unit_state);
        }
    }
    let rows: Vec<Value> = episodes
        .iter()
        .map(|episode| {
            let unit_state = effective_states
                .get(&(episode.season, episode.episode))
                .and_then(Option::as_ref);
            let (position_ms, played, progress_percent) = match unit_state {
                Some(state) => {
                    let percent = (!state.played)
                        .then(|| state.duration_ms.filter(|duration| *duration > 0))
                        .flatten()
                        .map(|duration| ((state.position_ms * 100) / duration).clamp(0, 100));
                    (state.position_ms, state.played, percent)
                }
                None => (0, false, None),
            };
            json!({
                "episode_number": episode.episode,
                "name": episode
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("第 {} 集", episode.episode)),
                "overview": episode.overview,
                "air_date": episode.air_date,
                "still_url": episode.still_url.as_deref().map(still_url),
                "owned": episode.owned,
                "file_ids": episode.file_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "position_ms": position_ms,
                "played": played,
                "progress_percent": progress_percent,
                "media_item_id": item_id,
                "season_number": episode.season,
                "runtime_minutes": episode.runtime_minutes,
                "rating": episode.rating,
            })
        })
        .collect();
    ok_list(rows).into_response()
}

/// A still reference is either a full URL or a TMDB path.
fn still_url(reference: &str) -> String {
    if reference.starts_with("http://")
        || reference.starts_with("https://")
        || reference.starts_with('/')
    {
        reference.to_string()
    } else {
        format!("https://image.tmdb.org/t/p/w300{reference}")
    }
}
