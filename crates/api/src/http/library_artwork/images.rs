//! Public native image reads: owned authorization facts first, bounded disk work second.

use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::http::library::library_visible;
use crate::http::{err, store_wait};
use crate::management::ApiState;

use super::image_content_type;
use super::image_response::conditional_image;

const IMAGE_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Kind {
    Poster,
    Backdrop,
    Still,
}

impl Kind {
    fn missing(self) -> Response {
        let message = match self {
            Self::Poster => "海报不存在",
            Self::Backdrop => "背景图不存在",
            Self::Still => "剧照不存在",
        };
        err(StatusCode::NOT_FOUND, "poster.missing", message)
    }
}

fn user_id(
    store: &crate::Store,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    headers: &HeaderMap,
) -> Option<domain::UserId> {
    user.and_then(|user| user.0).or_else(|| {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|token| store.user_id_by_token(token).ok().flatten())
    })
}

fn sidecar(
    path: &FsPath,
    kind: Kind,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Option<PathBuf> {
    if matches!(kind, Kind::Still) {
        return crate::episode_still::existing_with(path, files);
    }
    let name = if matches!(kind, Kind::Poster) {
        "poster.jpg"
    } else {
        "fanart.jpg"
    };
    let parent = path.parent()?;
    [
        Some(parent.join(name)),
        parent.parent().map(|p| p.join(name)),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| files.is_file(candidate))
}

async fn local_bytes(
    state: &ApiState,
    path: PathBuf,
    kind: Kind,
) -> Result<Option<Vec<u8>>, Response> {
    let files = state.file_stats.clone();
    let reservation = state
        .fs_work
        .reserve(IMAGE_WAIT)
        .await
        .map_err(|_| store_wait::busy_response())?;
    reservation
        .run_to_completion(move || {
            sidecar(&path, kind, files.as_ref()).and_then(|image| match std::fs::read(&image) {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    tracing::warn!(%error, path=%image.display(), "native image read failed");
                    None
                }
            })
        })
        .await
        .map_err(super::super::library::browse_work_error)
}

async fn artwork_bytes(
    state: ApiState,
    headers: HeaderMap,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    raw_file: String,
    kind: Kind,
) -> Response {
    let copied_headers = headers.clone();
    let captured = store_wait::store_read(&state, move |store| {
        let user = user_id(store, user, &copied_headers);
        let file = raw_file.parse::<domain::LedgerId>().ok()?;
        let coverage = store.file_coverage_by_id(file).ok().flatten()?;
        let media = store.get_media(coverage.file.media_id).ok().flatten()?;
        super::super::library::file_visible_to_user(store, &coverage.file, &media, user)
            .then_some((coverage, media))
    })
    .await;
    let (coverage, media) = match captured {
        Ok(Some(facts)) => facts,
        Ok(None) => return kind.missing(),
        Err(error) => return error.response(),
    };
    match local_bytes(&state, coverage.file.path.clone(), kind).await {
        Ok(Some(bytes)) => {
            let mime = image_content_type(&bytes);
            return conditional_image(&headers, bytes, mime, true);
        }
        Ok(None) => {}
        Err(response) => return response,
    };
    if matches!(kind, Kind::Still)
        || state.require_writable().is_err()
        || state.require_write_admitted().is_err()
    {
        return kind.missing();
    }
    let worker = state.clone();
    let path = coverage.file.path.clone();
    let fetch = state
        .http_work
        .run_to_completion(move || {
            if worker.require_writable().is_err() || worker.require_write_admitted().is_err() {
                return;
            }
            match kind {
                Kind::Poster => {
                    let _ = crate::poster_fetch::attach_poster(&worker, &media, &path);
                }
                Kind::Backdrop => {
                    let _ = crate::poster_fetch::attach_backdrop(&worker, &media, &path);
                }
                Kind::Still => {}
            }
        })
        .await;
    if let Err(error) = fetch {
        return super::super::library::browse_work_error(error);
    }
    match local_bytes(&state, coverage.file.path, kind).await {
        Ok(Some(bytes)) => {
            let mime = image_content_type(&bytes);
            conditional_image(&headers, bytes, mime, true)
        }
        Ok(None) => kind.missing(),
        Err(response) => response,
    }
}

pub(crate) async fn poster(
    State(state): State<ApiState>,
    headers: HeaderMap,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    Path(file): Path<String>,
) -> Response {
    artwork_bytes(state, headers, user, file, Kind::Poster).await
}

pub(crate) async fn fanart(
    State(state): State<ApiState>,
    headers: HeaderMap,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    Path(file): Path<String>,
) -> Response {
    artwork_bytes(state, headers, user, file, Kind::Backdrop).await
}

pub(crate) async fn stills(
    State(state): State<ApiState>,
    headers: HeaderMap,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    Path(file): Path<String>,
) -> Response {
    artwork_bytes(state, headers, user, file, Kind::Still).await
}

pub(crate) async fn library_cover(
    State(state): State<ApiState>,
    headers: HeaderMap,
    user: Option<axum::Extension<Option<domain::UserId>>>,
    Path(id): Path<String>,
) -> Response {
    let copied_headers = headers.clone();
    let capture = store_wait::store_read(&state, move |store| {
        let user = user_id(store, user, &copied_headers);
        store
            .get_library(&id)
            .ok()
            .flatten()
            .filter(|library| library_visible(store, library, user))
    })
    .await;
    let library = match capture {
        Ok(Some(library)) => library,
        Ok(None) => return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在"),
        Err(error) => return error.response(),
    };
    let cover = match state
        .cover_service()
        .ensure_cover(&state, &library.id)
        .await
    {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, library_id=%library.id, "failed to ensure library cover");
            None
        }
    };
    let reservation = match state.fs_work.reserve(IMAGE_WAIT).await {
        Ok(reservation) => reservation,
        Err(_) => return store_wait::busy_response(),
    };
    let result = reservation
        .run_to_completion(move || {
            if let Some(path) = cover {
                std::fs::read(path).map_err(|error| error.to_string())
            } else {
                crate::media_server_provider::default_library_cover(library.kind)
            }
        })
        .await;
    match result {
        Ok(Ok(bytes)) => {
            let mime = image_content_type(&bytes);
            conditional_image(&headers, bytes, mime, true)
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "读取 Library 封面失败");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(error) => super::super::library::browse_work_error(error),
    }
}
