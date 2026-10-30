//! Subscription title resolution + preview (`POST /subscriptions/title-preview`).
//!
//! A `title_ref` is a stable reference issued by Discover: exactly
//! `tmdb:movie:<digits>`, `tmdb:tv:<digits>` or `douban:<digits>`. Anything else
//! (legacy `tmdb:<id>`, three-segment douban, bare names, unknown sources/kinds,
//! empty IDs) is rejected with 400 `title_ref.invalid` before any catalog request
//! or persistent write. There is no bare-name search fallback.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Coverage, Media, MediaKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::http::{err, ok};
use crate::management::ApiState;

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";

/// Parsed stable reference to a media work.
pub(crate) enum TitleRef {
    Tmdb { kind: MediaKind, id: String },
    Douban(String),
}

/// Strict parse; `Err` carries the user-facing reason.
pub(crate) fn parse_title_ref(raw: &str) -> Result<TitleRef, String> {
    let numeric = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    match raw.split(':').collect::<Vec<_>>().as_slice() {
        ["tmdb", kind, id] if numeric(id) => {
            let kind = match *kind {
                "movie" => MediaKind::Movie,
                "tv" => MediaKind::Tv,
                _ => return Err(format!("title_ref 类型无效: {raw}")),
            };
            Ok(TitleRef::Tmdb {
                kind,
                id: (*id).to_string(),
            })
        }
        ["douban", id] if numeric(id) => Ok(TitleRef::Douban((*id).to_string())),
        _ => Err(format!(
            "title_ref 必须是 tmdb:movie:<id>、tmdb:tv:<id> 或 douban:<id>: {raw}"
        )),
    }
}

/// 400 response for a rejected reference.
pub(crate) fn invalid_title_ref(reason: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "title_ref.invalid", reason)
}

/// Resolve a parsed reference via the catalog. `None` = not found.
pub(crate) fn resolve_title(state: &ApiState, title: &TitleRef) -> Result<Option<Media>, String> {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    match title {
        TitleRef::Tmdb { kind, id } => catalog.details(*kind, id),
        TitleRef::Douban(id) => match catalog.source("douban") {
            Some(douban) => douban.details(MediaKind::Movie, id),
            None => Ok(None),
        },
    }
}

#[derive(Deserialize)]
pub(crate) struct TitlePreviewInput {
    title_ref: String,
    #[serde(default)]
    season: Option<u32>,
}

pub(crate) async fn preview(
    State(state): State<ApiState>,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    Json(body): Json<TitlePreviewInput>,
) -> Response {
    let worker = state.clone();
    match state
        .http_work
        .run(super::library::BROWSE_DEADLINE, move || {
            preview_blocking(&worker, user_id, body)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => super::library::browse_work_error(error),
    }
}

fn preview_blocking(
    state: &ApiState,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    body: TitlePreviewInput,
) -> Response {
    let title = match parse_title_ref(&body.title_ref) {
        Ok(title) => title,
        Err(reason) => return invalid_title_ref(&reason),
    };
    let resolved = match resolve_title(state, &title).unwrap_or(None) {
        Some(media) => media,
        None => {
            return ok(json!({
                "status": "not_found", "media": Value::Null, "seasons": [],
                "existing_subscription_id": Value::Null, "movie_owned": false,
                "suggested_seasons": [], "candidates": [],
            }))
            .into_response();
        }
    };
    let Some((media, existing, owned_seasons, movie_owned)) =
        preview_facts(state, resolved, user_id.and_then(|u| u.0), body.season)
    else {
        return super::store_wait::busy_response();
    };
    let seasons = preview_seasons(state, &media, &owned_seasons);
    ok(json!({
        "status": "ready",
        "media": {
            "media_item_id": media.id.to_string(),
            "kind": media.kind.as_str(),
            "tmdb_id": media.tmdb_id,
            "douban_id": media.douban_id,
            "title": media.title,
            "original_title": media.original_title,
            "year": media.year,
            "poster_url": media.tmdb_id.as_ref().map(|_| TMDB_POSTER.to_string()),
            "status": Value::Null,
        },
        "seasons": seasons,
        "existing_subscription_id": existing,
        "movie_owned": movie_owned,
        "suggested_seasons": [],
        "candidates": [],
    }))
    .into_response()
}

type PreviewFacts = (
    Media,
    Option<String>,
    std::collections::HashMap<u32, usize>,
    bool,
);

fn preview_facts(
    state: &ApiState,
    resolved: Media,
    current_user_id: Option<domain::UserId>,
    season: Option<u32>,
) -> Option<PreviewFacts> {
    super::store_wait::with_store_read(state, |store| {
        // 收敛到库内已有 Media 行（按 tmdb 别名 **带 kind**）：相同数字的
        // movie/TV id 不得互相顶替（P2：preview tmdb:tv:123 不应命中 movie:123）。
        let media = resolved
            .tmdb_id
            .as_deref()
            .and_then(|id| {
                store
                    .get_media_by_alias_kind("tmdb", id, Some(resolved.kind))
                    .ok()
                    .flatten()
            })
            .unwrap_or(resolved);
        let media_id = media.id;
        let existing = store
            .list_all_subscribes()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                if let Some(uid) = current_user_id {
                    s.user_id == uid
                } else {
                    true
                }
            })
            .filter(|s| s.media_id == media_id)
            .find(|s| match (&s.coverage, season) {
                (domain::Coverage::Tv { season, .. }, Some(target_season)) => {
                    *season == target_season
                }
                _ => true,
            })
            .map(|s| s.id.to_string());

        let ledger = store.file_coverage_for_media(media_id).unwrap_or_default();
        let (owned_seasons, has_owned) = owned_episode_counts(&ledger, media_id);
        let movie_owned = media.kind == MediaKind::Movie && has_owned;
        (media, existing, owned_seasons, movie_owned)
    })
    .ok()
}

fn preview_seasons(
    state: &ApiState,
    media: &Media,
    owned_seasons: &std::collections::HashMap<u32, usize>,
) -> Vec<Value> {
    // TV：季总览（TMDB seasons + 已播/已有计数）
    // 关键（D15）：在调用 external catalog 之前已完全释放 store 锁，杜绝后台 try_lock_for 超时退化
    if media.kind == MediaKind::Tv {
        let tmdb_id = media.tmdb_id.clone().unwrap_or_default();
        let rows = state.catalog.tv_seasons(&tmdb_id).unwrap_or_default();
        tracing::info!(tmdb_id = %tmdb_id, rows_count = rows.len(), "title-preview 正在拉取 TV seasons");
        let today = today_rfc3339();
        rows.iter()
            .map(|season| {
                let aired = season
                    .air_date
                    .as_deref()
                    .map(|date| date <= today.as_str())
                    .unwrap_or(false);
                let owned = owned_seasons
                    .get(&season.season_number)
                    .copied()
                    .unwrap_or(0);
                json!({
                    "season_number": season.season_number,
                    "name": season.name,
                    "air_date": season.air_date,
                    "episode_count": season.episode_count,
                    "aired_count": if aired { season.episode_count.unwrap_or(0) } else { 0 },
                    "owned_count": owned,
                })
            })
            .collect()
    } else {
        Vec::new()
    }
}

/// Count already-owned episodes per season for `media_id`, plus whether any file
/// of that media is present at all (`has_owned` drives `movie_owned`).
fn owned_episode_counts(
    coverage: &[domain::FileCoverage],
    media_id: domain::MediaId,
) -> (std::collections::HashMap<u32, usize>, bool) {
    let mut owned_seasons: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut has_owned = false;
    for row in coverage.iter().filter(|row| row.file.media_id == media_id) {
        has_owned = true;
        // 一个物理文件可覆盖多个 unit；这里按 unit 逐个计入，每一集都保留。
        for unit in &row.units {
            if let Some((season, _)) = unit.episode_coordinates() {
                *owned_seasons.entry(season).or_default() += 1;
            }
        }
    }
    (owned_seasons, has_owned)
}

/// `YYYY-MM-DD` for today (UTC), for aired-count comparison with TMDB dates.
fn today_rfc3339() -> String {
    let s = crate::store::now_rfc3339();
    s.split('T').next().unwrap_or(&s).to_string()
}

/// Build a `{kind, title, tmdb_id, ...}` media identity from a resolved Media,
/// used by `create_subscription` when the frontend sends `title_ref` instead
/// of a full media body.
pub(crate) fn media_identity(media: &Media) -> Value {
    json!({
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "tmdb_id": media.tmdb_id,
        "douban_id": media.douban_id,
        "tvdb_id": media.tvdb_id,
        "bangumi_id": media.bangumi_id,
        "anilist_id": media.anilist_id,
    })
}

/// Coverage for a resolved Media when the frontend did not send one
/// (movie → Movie; tv → first season, open episode window).
pub(crate) fn default_coverage(kind: MediaKind) -> Coverage {
    match kind {
        MediaKind::Movie | MediaKind::Video => Coverage::Movie,
        MediaKind::Tv => Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: None,
        },
    }
}
