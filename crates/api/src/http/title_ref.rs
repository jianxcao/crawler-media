//! Subscription title resolution + preview (`POST /subscriptions/title-preview`).
//!
//! Revives the subscribe dialog: a stable `title_ref` (`tmdb:movie:603`,
//! `tmdb:603`, or a bare title) is resolved against the catalog into a Media,
//! with existing-subscription / owned / season-overview facts. Douban refs
//! (`douban:<id>`) cannot be resolved without a provider fanout (U3); they
//! return `not_found`.

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
#[allow(dead_code)] // Douban's payload is only ever parsed, never read (no fanout yet, U3).
pub(crate) enum TitleRef {
    Tmdb { kind: MediaKind, id: String },
    Douban(String),
    Bare(String),
}

pub(crate) fn parse_title_ref(raw: &str) -> TitleRef {
    let parts: Vec<&str> = raw.split(':').collect();
    match parts.as_slice() {
        ["tmdb", kind, id] => {
            let kind = match *kind {
                "movie" => MediaKind::Movie,
                "tv" => MediaKind::Tv,
                _ => return TitleRef::Bare(raw.to_string()),
            };
            TitleRef::Tmdb {
                kind,
                id: (*id).to_string(),
            }
        }
        ["tmdb", id] => TitleRef::Tmdb {
            kind: MediaKind::Movie,
            id: (*id).to_string(),
        },
        // 前端 titleRef() 生成 `douban:movie:1292052`（三/两段皆可）。
        ["douban", _, id] | ["douban", id] => TitleRef::Douban((*id).to_string()),
        _ => TitleRef::Bare(raw.to_string()),
    }
}

/// Resolve a title_ref to a Media via the catalog. `None` = not found.
pub(crate) fn resolve_title(state: &ApiState, raw: &str) -> Result<Option<Media>, String> {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    match parse_title_ref(raw) {
        TitleRef::Tmdb { kind, id } => catalog.details(kind, &id),
        TitleRef::Douban(id) => match catalog.source("douban") {
            Some(douban) => douban.details(MediaKind::Movie, &id),
            None => Ok(None),
        },
        TitleRef::Bare(title) => {
            // One clear hit → take it; several → the caller shows candidates.
            let movies = catalog.search_movie(&title).unwrap_or_default();
            let shows = catalog.search_tv(&title).unwrap_or_default();
            let mut hits: Vec<&media::CatalogHit> = movies.iter().chain(shows.iter()).collect();
            hits.retain(|hit| hit.media.tmdb_id.is_some());
            Ok(match hits.len() {
                1 => Some(hits[0].media.clone()),
                _ => None,
            })
        }
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
    if body.title_ref.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "subscribe.preview",
            "title_ref 必填",
        );
    }
    // 裸标题多命中 → ambiguous（候选墙）；单命中/精确引用 → ready。
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let resolved = match parse_title_ref(&body.title_ref) {
        TitleRef::Tmdb { kind, id } => catalog.details(kind, &id).unwrap_or(None),
        TitleRef::Douban(id) => catalog
            .source("douban")
            .and_then(|douban| douban.details(MediaKind::Movie, &id).ok().flatten()),
        TitleRef::Bare(title) => {
            let movies = catalog.search_movie(&title).unwrap_or_default();
            let shows = catalog.search_tv(&title).unwrap_or_default();
            let mut hits: Vec<&media::CatalogHit> = movies.iter().chain(shows.iter()).collect();
            hits.retain(|hit| hit.media.tmdb_id.is_some());
            if hits.len() > 1 {
                let candidates: Vec<Value> = hits
                    .iter()
                    .map(|hit| {
                        let kind = hit.media.kind;
                        let tmdb_id = hit.media.tmdb_id.clone().unwrap_or_default();
                        let poster = hit
                            .poster_path
                            .as_deref()
                            .map(|p| format!("{TMDB_POSTER}{p}"));
                        json!({
                            "tmdb_id": tmdb_id,
                            "title_ref": format!("tmdb:{}:{tmdb_id}", kind.as_str()),
                            "title": hit.media.title,
                            "original_title": hit.media.original_title,
                            "year": hit.media.year,
                            "poster_url": poster,
                        })
                    })
                    .collect();
                return ok(json!({
                    "status": "ambiguous",
                    "media": Value::Null,
                    "seasons": [],
                    "existing_subscription_id": Value::Null,
                    "movie_owned": false,
                    "suggested_seasons": [],
                    "candidates": candidates,
                }))
                .into_response();
            }
            hits.first().map(|hit| hit.media.clone())
        }
    };
    let Some(resolved) = resolved else {
        return ok(json!({
            "status": "not_found",
            "media": Value::Null,
            "seasons": [],
            "existing_subscription_id": Value::Null,
            "movie_owned": false,
            "suggested_seasons": [],
            "candidates": [],
        }))
        .into_response();
    };
    let current_user_id = user_id.and_then(|u| u.0);
    let (media, existing, owned_seasons, movie_owned) = {
        let store = state.store.lock();
        // 收敛到库内已有 Media 行（按 tmdb 别名 **带 kind**）：相同数字的
        // movie/TV id 不得互相顶替（P2：preview tmdb:tv:123 不应命中 movie:123）。
        let media = resolved
            .tmdb_id
            .as_deref()
            .and_then(|id| {
                store
                    .get_media_by_alias_kind("tmdb_id", id, Some(resolved.kind))
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
            .find(|s| match (&s.coverage, body.season) {
                (domain::Coverage::Tv { season, .. }, Some(target_season)) => {
                    *season == target_season
                }
                _ => true,
            })
            .map(|s| s.id.to_string());

        let ledger = store.list_ledger().unwrap_or_default();
        let mut owned_seasons: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        let mut has_owned = false;
        for row in ledger.iter().filter(|row| row.media_id == media_id) {
            has_owned = true;
            if let Some(s) = row.season {
                *owned_seasons.entry(s).or_default() += 1;
            }
        }
        let movie_owned = media.kind == MediaKind::Movie && has_owned;
        (media, existing, owned_seasons, movie_owned)
    };

    // TV：季总览（TMDB seasons + 已播/已有计数）
    // 关键（D15）：在调用 external catalog 之前已完全释放 store 锁，杜绝后台 try_lock_for 超时退化
    let seasons: Vec<Value> = if media.kind == MediaKind::Tv {
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
    };

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
            "poster_url": media.tmdb_id.as_ref().map(|_| format!("{TMDB_POSTER}")),
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
