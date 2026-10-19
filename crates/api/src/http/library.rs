//! Library browse: Library listing and item views rendered from the `library_items`
//! summaries. Entity CRUD lives in `library_config`, scans/refresh in `library_scan`,
//! artwork in `library_artwork`, admin views in `library_admin`.

mod probe;
pub(crate) use probe::{
    enqueue_probes_for_paths, enqueue_probes_for_rows, ensure_probe_enqueued, probe_item,
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
use crate::store::{library_query, Library};

mod assets;
mod cover;
pub(crate) mod page;
pub(crate) use assets::{library_json, poster_path, poster_path_with, preferred_row, rows_in_library};
pub(crate) use cover::{default_cover_options, is_legacy_fanart_copy, render_cover_jpeg};

pub(crate) const BROWSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn browse_work_error(error: crate::blocking_budget::BlockingError) -> Response {
    use crate::blocking_budget::BlockingError;
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "library.timeout",
            "浏览请求超时",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}
/// HTTP query parameters → the store browse filter. A value the filter cannot express is an
/// error the handler turns into 400: a bogus filter must never silently return everything.
/// Shared by the item page, the first-letter index, gallery and facets so one request shape
/// cannot mean different things on different endpoints.
pub(crate) fn browse_filter_from_query(
    query: &HashMap<String, String>,
) -> Result<library_query::BrowseFilter, String> {
    let list = |keys: &[&str]| -> Vec<String> {
        keys.iter()
            .find_map(|key| query.get(*key))
            .map(|raw| {
                raw.split(',')
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let flag = |key: &str| -> Result<Option<bool>, String> {
        match query.get(key) {
            Some(raw) => raw
                .parse::<bool>()
                .map(Some)
                .map_err(|_| format!("{key} must be true or false")),
            None => Ok(None),
        }
    };
    let rating = match query.get("rating_gte") {
        Some(raw) => {
            let value = raw
                .parse::<f64>()
                .map_err(|_| "rating_gte must be numeric".to_string())?;
            if !value.is_finite() || !(0.0..=10.0).contains(&value) {
                return Err("rating_gte must be between 0 and 10".into());
            }
            Some(value)
        }
        None => None,
    };
    Ok(library_query::BrowseFilter {
        genres: list(&["genres", "g"]),
        countries: list(&["countries", "c"]),
        decades: list(&["decades", "d"]),
        runtimes: list(&["runtimes", "rt"]),
        languages: list(&["languages", "lang"]),
        resolutions: list(&["resolutions", "res"]),
        stock: list(&["stock"]),
        hdr: flag("hdr")?,
        min_rating: rating,
        identity: query.get("identity").cloned(),
        watch: query.get("watch").or_else(|| query.get("w")).cloned(),
        unwatched_first: query
            .get("unwatched_first")
            .is_some_and(|raw| raw == "true" || raw == "1"),
        require_image: false,
    })
}

/// The page/sort half of a browse request, shared by items, index and gallery.
pub(crate) fn browse_order_from_query(
    query: &HashMap<String, String>,
) -> (library_query::BrowseOrder, bool) {
    let order = match query.get("sort").map(String::as_str) {
        Some("title") => library_query::BrowseOrder::Title,
        Some("added_at") => library_query::BrowseOrder::AddedAt,
        Some("release_date" | "release_date_asc") => library_query::BrowseOrder::ReleaseDate,
        Some("rating") => library_query::BrowseOrder::Rating,
        Some("runtime") => library_query::BrowseOrder::Runtime,
        Some("size") => library_query::BrowseOrder::Size,
        Some("resolution") => library_query::BrowseOrder::Resolution,
        Some("last_played") => library_query::BrowseOrder::LastPlayed,
        Some("random") => library_query::BrowseOrder::Random,
        _ => library_query::BrowseOrder::Title,
    };
    let descending = match query.get("order").map(|value| value.as_str()) {
        Some("desc") => true,
        Some("asc") => false,
        // Same defaults the wall has always used; `release_date_asc` is the explicit
        // ascending spelling of the release-date sort.
        _ => {
            query.get("sort").map(String::as_str) != Some("release_date_asc")
                && matches!(
                    order,
                    library_query::BrowseOrder::AddedAt
                        | library_query::BrowseOrder::ReleaseDate
                        | library_query::BrowseOrder::Rating
                        | library_query::BrowseOrder::Size
                        | library_query::BrowseOrder::LastPlayed
                )
        }
    };
    (order, descending)
}

/// Map a store failure of a browse read to a response, keeping filter rejections a 400.
pub(crate) fn browse_store_error(error: store::StoreError) -> Response {
    match error {
        store::StoreError::Invalid(message) => {
            err(StatusCode::BAD_REQUEST, "library.invalid", &message)
        }
        error => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
    }
}
/// Resolve one ledger row to exactly one Library. The longest matching root
/// wins, so a nested Library does not leak through a broader parent root. Two
/// Libraries with equally-specific roots are ambiguous and therefore own
/// nothing until the configuration is fixed.
pub(crate) fn library_for_row(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> Option<Library> {
    store
        .library_for_path(FsPath::new(&row.path), media.kind)
        .ok()
        .flatten()
}

/// Destructive file operations must not fall back to the default Library.
pub(crate) fn library_for_row_strict(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> Option<Library> {
    store
        .library_for_path_strict(FsPath::new(&row.path), media.kind)
        .ok()
        .flatten()
}

pub(crate) fn row_visible_to_user(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
    user_id: Option<domain::UserId>,
) -> bool {
    library_for_row(store, row, media)
        .is_some_and(|library| library_visible(store, &library, user_id))
}

/// Whether a library is visible to the given user: admins see everything
/// (subject to the library's admin_visible flag for browse scope); members see
/// "everyone" libraries plus "selected" ones that list them.
pub(crate) fn library_visible(
    store: &crate::Store,
    library: &Library,
    user_id: Option<domain::UserId>,
) -> bool {
    if !store.library_accepts_work(&library.id).unwrap_or(false) {
        return false;
    }
    let Some(user_id) = user_id else {
        return true;
    };
    if store
        .user_role(user_id)
        .as_deref()
        .map(|r| r == "admin")
        .unwrap_or(false)
    {
        return library.admin_visible;
    }
    if library.access_mode == "everyone" {
        return true;
    }
    library.member_ids.contains(&user_id)
}

pub(crate) fn library_visible_with_role(
    store: &crate::Store,
    library: &Library,
    user_id: Option<domain::UserId>,
    is_admin: bool,
) -> bool {
    if !store.library_accepts_work(&library.id).unwrap_or(false) {
        return false;
    }
    let Some(user_id) = user_id else {
        return true;
    };
    if is_admin {
        return library.admin_visible;
    }
    library.access_mode == "everyone" || library.member_ids.contains(&user_id)
}

pub(crate) fn missing_library() -> Response {
    err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在")
}

pub(crate) fn artwork_url(kind: &str, ledger_id: domain::LedgerId) -> String {
    format!("/{kind}/{ledger_id}")
}

pub(crate) fn require_visible_library(
    store: &crate::Store,
    id: &str,
    user_id: Option<domain::UserId>,
) -> Result<Library, Response> {
    let Some(library) = store.get_library(id).ok().flatten() else {
        return Err(missing_library());
    };
    if !library_visible(store, &library, user_id) {
        return Err(missing_library());
    }
    Ok(library)
}

/// 长图（fanart.jpg）向上查找：与 poster_path 同一套布局兼容。
pub(crate) fn backdrop_path(row: &domain::LedgerRow) -> Option<PathBuf> {
    backdrop_path_with(row, &crate::browse_file_read::LocalBrowseFileRead)
}

/// 与 [`backdrop_path`] 同一套布局，存在性判断走注入的浏览文件 seam。
pub(crate) fn backdrop_path_with(
    row: &domain::LedgerRow,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Option<PathBuf> {
    let file_dir = FsPath::new(&row.path).parent()?;
    for dir in [file_dir, file_dir.parent().unwrap_or(file_dir)] {
        let art = dir.join("fanart.jpg");
        if files.is_file(&art) {
            return Some(art);
        }
    }
    None
}

/// 解析媒体库封面：**只读**已存在的版本文件，不生成、不写盘、不写 DB。
/// 自动生成统一走 `LibraryCoverService::ensure_cover`。
pub(crate) fn library_cover_path(store: &crate::Store, library: &Library) -> Option<PathBuf> {
    // 0. 用户显式删除（"none"/空串）时不回填
    match library.cover_path.as_deref() {
        Some("none") | Some("") => return None,
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        None => {}
    }
    // 1. legacy 固定路径 `library-{id}.jpg` 仅读、不 unlink
    let legacy = store
        .library_covers_dir()
        .join(format!("library-{}.jpg", library.id));
    legacy.is_file().then_some(legacy)
}

fn library_cover_checked_key(library_id: &str) -> String {
    format!("library_cover_checked.{library_id}")
}

pub(crate) fn clear_library_cover_checked(store: &crate::Store, library_id: &str) {
    let key = library_cover_checked_key(library_id);
    match store.get_setting(&key) {
        Ok(Some(value)) if value == "auto-v2" => {
            if let Err(error) = store.delete_setting(&key) {
                tracing::warn!(%error, library_id, "failed to clear library cover check marker");
            }
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, library_id, "failed to read library cover check marker");
        }
    }
}

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
        state.clone(), user_id, &roster, is_admin,
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
    // 显式分页参数先校验：越界直接 400；省略 limit 仍是"返回全部"。
    let window = match crate::http::library::page::PageWindow::parse(&query) {
        Ok(window) => window,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    let lib_q = library_query::LibraryQuery {
        library_id: lib_id,
        user_id,
        filter,
        order,
        descending,
        page: library_query::BrowsePage::Offset {
            start: window.offset,
            limit: window.limit.unwrap_or(usize::MAX / 2),
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
    }))
    .into_response()
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
    let video = match details.video_json.as_deref() {
        Some(raw) => Some(
            serde_json::from_str::<library::VideoTrack>(raw).map_err(|error| {
                store::StoreError::Invalid(format!("file details video: {error}"))
            })?,
        ),
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
    let (episodes, progress) = {
        let store = state.store.lock();
        if let Err(response) = require_visible_library(&store, &id, user_id) {
            return response;
        }
        match store.get_library_item(lib_id, media_id) {
            Ok(Some(_)) => {}
            Ok(None) => return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"),
            Err(error) => return browse_store_error(error),
        }
        let episodes = match store.library_episodes(lib_id, media_id, season, "") {
            Ok(episodes) => episodes,
            Err(error) => return browse_store_error(error),
        };
        let progress = match user_id {
            Some(user_id) => match store.playback_units_for_media(user_id, &[media_id]) {
                Ok(rows) => rows,
                Err(error) => return browse_store_error(error),
            },
            None => Vec::new(),
        };
        (episodes, progress)
    };
    let rows: Vec<Value> = episodes
        .iter()
        .map(|episode| {
            let unit_state = progress
                .iter()
                .find(|unit| {
                    unit.season >= 0
                        && unit.episode >= 0
                        && unit.season as u32 == episode.season
                        && unit.episode as u32 == episode.episode
                })
                .map(|unit| &unit.state);
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
                "watched": played,
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
    if reference.starts_with("http://") || reference.starts_with("https://") {
        reference.to_string()
    } else {
        format!("https://image.tmdb.org/t/p/w300{reference}")
    }
}

