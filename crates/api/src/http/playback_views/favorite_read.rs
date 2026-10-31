//! Shared favorite page scope, selection and bounded hydration.
use std::collections::HashMap;

use axum::{http::StatusCode, response::Response};
use domain::{FileCoverage, Media, MediaId, UserId};
use store::{PlaybackViewPage, PlaybackViewQuery, Store, StoreError};

use crate::http::err;

pub(super) struct Selection {
    pub page: PlaybackViewPage,
    pub media: HashMap<MediaId, Media>,
    pub coverage: HashMap<MediaId, Vec<FileCoverage>>,
    pub libraries: Vec<domain::LibraryId>,
}

pub(super) fn query(
    raw: &HashMap<String, String>,
    user: UserId,
) -> Result<PlaybackViewQuery, Response> {
    if raw
        .keys()
        .any(|key| !["limit", "after", "sort", "order", "unwatched_first"].contains(&key.as_str()))
    {
        return Err(invalid("未知收藏查询参数"));
    }
    let sort = raw
        .get("sort")
        .map(String::as_str)
        .unwrap_or("favorited_at");
    let order = match sort {
        "favorited_at" => store::FavoriteOrder::FavoritedAt,
        "title" => store::FavoriteOrder::Title,
        "year" => store::FavoriteOrder::Year,
        _ => return Err(invalid("sort 必须是 favorited_at、title 或 year")),
    };
    let descending = match raw.get("order").map(String::as_str) {
        None => sort != "title",
        Some("desc") => true,
        Some("asc") => false,
        _ => return Err(invalid("order 必须是 asc 或 desc")),
    };
    let limit = raw
        .get("limit")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| invalid("limit 必须是非负整数"))?
        .unwrap_or(20)
        .clamp(1, 200);
    let unwatched_first = match raw.get("unwatched_first").map(String::as_str) {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        _ => return Err(invalid("unwatched_first 必须是布尔值")),
    };
    Ok(PlaybackViewQuery {
        user,
        libraries: Vec::new(),
        limit,
        after: raw.get("after").cloned(),
        order,
        descending,
        unwatched_first,
    })
}

pub(super) fn invalid(message: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "favorites.invalid", message)
}

pub(super) fn store_error(error: StoreError) -> Response {
    match error {
        StoreError::Invalid(message) => invalid(&message),
        error => {
            tracing::error!(%error, "Favorite selection failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "favorites.store",
                "收藏读取失败",
            )
        }
    }
}

pub(super) fn scope(store: &Store, query: &mut PlaybackViewQuery) -> Result<(), StoreError> {
    query.libraries = crate::visible_scope::VisibleLibraries::load(store, Some(query.user))?.ids();
    Ok(())
}

pub(super) fn capture(
    store: &Store,
    mut query: PlaybackViewQuery,
) -> Result<Selection, StoreError> {
    scope(store, &mut query)?;
    let page = store.playback_favorite_page(&query)?;
    let ids = page
        .items
        .iter()
        .map(|row| row.media_id)
        .collect::<Vec<_>>();
    let media = store.media_batch(&ids)?;
    let mut coverage: HashMap<_, Vec<_>> = HashMap::new();
    for row in store.file_coverage_for_media_batch(&ids)? {
        if row.file.publication == domain::FilePublication::Published
            && query.libraries.contains(&row.file.library_id)
        {
            coverage.entry(row.file.media_id).or_default().push(row);
        }
    }
    Ok(Selection {
        page,
        media,
        coverage,
        libraries: query.libraries,
    })
}

/// The management favorite wall includes published missing files. Its validation
/// therefore compares captured file facts rather than applying playback's
/// present-only source validator.
pub(super) struct Snapshot {
    libraries: Vec<domain::LibraryId>,
    media: Vec<MediaId>,
    files: HashMap<domain::LedgerId, domain::LibraryFile>,
}

impl Selection {
    pub(super) fn snapshot(&self, libraries: Vec<domain::LibraryId>) -> Snapshot {
        Snapshot {
            libraries,
            media: self.page.items.iter().map(|row| row.media_id).collect(),
            files: self
                .coverage
                .values()
                .flatten()
                .map(|row| (row.file.file_id, row.file.clone()))
                .collect(),
        }
    }
}

pub(super) fn validate(
    store: &Store,
    user: UserId,
    snapshot: &Snapshot,
) -> Result<bool, StoreError> {
    let libraries = crate::visible_scope::VisibleLibraries::load(store, Some(user))?.ids();
    if libraries != snapshot.libraries {
        return Ok(false);
    }
    let files = store
        .file_coverage_for_media_batch(&snapshot.media)?
        .into_iter()
        .filter(|row| row.file.publication == domain::FilePublication::Published)
        .filter(|row| libraries.contains(&row.file.library_id))
        .map(|row| (row.file.file_id, row.file))
        .collect::<HashMap<_, _>>();
    Ok(files == snapshot.files)
}

pub(super) fn changed() -> Response {
    err(
        StatusCode::CONFLICT,
        "favorites.changed",
        "收藏来源已变化，请刷新",
    )
}
