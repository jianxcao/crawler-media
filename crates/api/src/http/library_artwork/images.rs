//! Public native image reads: owned authorization facts first, bounded disk work second.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::http::library::library_visible;
use crate::http::{err, store_wait};
use crate::management::ApiState;

use super::image_content_type;
use super::image_response::{IMAGE_READ, IMAGE_WAIT, conditional_image};

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
        .artwork_reads
        .reserve(IMAGE_WAIT)
        .await
        .map_err(|_| store_wait::busy_response())?;
    reservation
        .run(IMAGE_READ, move || {
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
            .then_some(coverage)
    })
    .await;
    let coverage = match captured {
        Ok(Some(facts)) => facts,
        Ok(None) => return kind.missing(),
        Err(error) => return error.response(),
    };
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
    let path = library
        .cover_path
        .as_deref()
        .filter(|path| !matches!(*path, "none" | ""));
    if path.is_none() {
        if library.cover_path.is_none() {
            state
                .cover_service()
                .refresh_in_background(&state, &library.id);
        }
        return match crate::media_server_provider::default_library_cover(library.kind) {
            Ok(bytes) => conditional_image(&headers, bytes, "image/jpeg", true),
            Err(error) => {
                tracing::error!(%error, "Default Library cover unavailable");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
    }
    let reservation = match state.artwork_reads.reserve(IMAGE_WAIT).await {
        Ok(reservation) => reservation,
        Err(_) => return store_wait::busy_response(),
    };
    let refresh_id = library.id.clone();
    let result = reservation
        .run(IMAGE_READ, move || {
            if let Some(path) = library.cover_path.as_deref().filter(|path| !matches!(*path, "none" | "")) {
                match std::fs::read(path) {
                    Ok(bytes) => return Ok((bytes, false)),
                    Err(error) => tracing::warn!(%error, %path, "cached Library cover unavailable; serving default"),
                }
            }
            crate::media_server_provider::default_library_cover(library.kind).map(|bytes| (bytes, true))
        })
        .await;
    match result {
        Ok(Ok((bytes, refresh))) => {
            if refresh {
                state
                    .cover_service()
                    .refresh_in_background(&state, &refresh_id);
            }
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
