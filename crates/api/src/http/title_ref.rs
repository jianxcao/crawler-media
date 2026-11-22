//! Subscription title resolution + preview (`POST /subscriptions/title-preview`).
//!
//! A `title_ref` is a stable reference issued by Discover: exactly
//! `tmdb:movie:<digits>`, `tmdb:tv:<digits>` or `douban:<digits>`. Anything else
//! (legacy `tmdb:<id>`, three-segment douban, bare names, unknown sources/kinds,
//! empty IDs) is rejected with 400 `title_ref.invalid` before any catalog request
//! or persistent write. There is no bare-name search fallback.

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Coverage, Media, MediaId, MediaKind};
use media::CatalogHit;
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

pub(crate) enum TmdbAnchorOutcome {
    None,
    Unique(Media),
    Ambiguous(Vec<CatalogHit>),
}

enum PreviewOutcome {
    NotFound,
    Ambiguous(Vec<Value>),
    Ready(Media),
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

fn catalog_source<'a>(catalog: &'a dyn Catalog, name: &str) -> Option<&'a dyn Catalog> {
    catalog
        .source(name)
        .or_else(|| (catalog.source_name() == name).then_some(catalog))
}

/// Resolve a parsed reference via the catalog. `None` = not found.
pub(crate) fn resolve_title(state: &ApiState, title: &TitleRef) -> Result<Option<Media>, String> {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    match title {
        TitleRef::Tmdb { kind, id } => catalog.details(*kind, id),
        TitleRef::Douban(id) => {
            if let Some(local) = local_douban_media(state, id) {
                return Ok(Some(local));
            }
            let Some(douban_media) = fetch_douban_media(catalog, id)? else {
                return Ok(None);
            };
            match anchor_douban_to_tmdb(catalog, &douban_media) {
                TmdbAnchorOutcome::Unique(anchored) => Ok(Some(anchored)),
                TmdbAnchorOutcome::None | TmdbAnchorOutcome::Ambiguous(_) => Ok(Some(douban_media)),
            }
        }
    }
}

fn local_douban_media(state: &ApiState, douban_id: &str) -> Option<Media> {
    let local = super::store_wait::with_store_read(state, &[store::Db::Library], |store| {
        store.get_media_by_alias("douban", douban_id).ok().flatten()
    })
    .ok()
    .flatten()?;
    let tmdb_id = local.tmdb_id.as_deref()?;
    let mut media = state
        .catalog
        .details(local.kind, tmdb_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| local.clone());
    media.douban_id = Some(douban_id.to_string());
    merge_aliases(&mut media.aliases, &local.aliases, &media.title);
    Some(media)
}

fn fetch_douban_media(catalog: &dyn Catalog, douban_id: &str) -> Result<Option<Media>, String> {
    if let Some(douban) = catalog_source(catalog, "douban") {
        let mut media = douban.details(MediaKind::Movie, douban_id)?;
        if let Some(ref mut m) = media {
            m.douban_id = Some(douban_id.to_string());
        }
        return Ok(media);
    }
    match catalog.douban_subject_with_credits(douban_id) {
        Ok(pair) => Ok(media_from_rexxar_subject(douban_id, &pair.subject)),
        Err(_) => Ok(None),
    }
}

pub(crate) fn media_from_rexxar_subject(douban_id: &str, v: &Value) -> Option<Media> {
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let original_title = v
        .get("original_title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != title)
        .map(str::to_string);
    let year = v.get("year").and_then(|y| {
        y.as_str()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .or_else(|| y.as_u64().and_then(|n| u16::try_from(n).ok()))
    });
    let is_tv = is_rexxar_tv(v);
    let aliases = v
        .get("aka")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Media {
        id: MediaId::new(),
        kind: if is_tv {
            MediaKind::Tv
        } else {
            MediaKind::Movie
        },
        title,
        year,
        original_title,
        aliases,
        tmdb_id: None,
        douban_id: Some(douban_id.to_string()),
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    })
}

pub(crate) fn is_rexxar_tv(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("tv")
        || v.get("subtype").and_then(Value::as_str) == Some("tv")
        || v.get("is_tv").and_then(Value::as_bool) == Some(true)
        || rexxar_episodes_count(v).is_some_and(|n| n > 0)
}

pub(crate) fn rexxar_episodes_count(v: &Value) -> Option<u32> {
    v.get("episodes_count")
        .and_then(Value::as_u64)
        .or_else(|| {
            v.get("episodes_count")
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
}

pub(crate) fn anchor_douban_to_tmdb(
    catalog: &dyn Catalog,
    douban_media: &Media,
) -> TmdbAnchorOutcome {
    let Some(tmdb) = catalog_source(catalog, "tmdb") else {
        return TmdbAnchorOutcome::None;
    };
    let queries = search_queries_for_douban(douban_media);
    let douban_names = normalized_media_names(douban_media);
    if queries.is_empty() || douban_names.is_empty() {
        return TmdbAnchorOutcome::None;
    }

    let primary = douban_media.kind;
    let secondary = if primary == MediaKind::Tv {
        MediaKind::Movie
    } else {
        MediaKind::Tv
    };

    let mut matched = collect_matching_hits(tmdb, primary, &queries, douban_media, &douban_names);
    if matched.is_empty() {
        matched = collect_matching_hits(tmdb, secondary, &queries, douban_media, &douban_names);
    }
    if matched.is_empty() {
        return TmdbAnchorOutcome::None;
    }

    matched.sort_by_key(|a| std::cmp::Reverse(candidate_rank(douban_media, a)));
    if matched.len() == 1
        || candidate_rank(douban_media, &matched[0]) > candidate_rank(douban_media, &matched[1])
    {
        let winner = matched.remove(0);
        return TmdbAnchorOutcome::Unique(finalize_anchored_media(tmdb, douban_media, winner));
    }
    TmdbAnchorOutcome::Ambiguous(matched)
}

fn search_queries_for_douban(media: &Media) -> Vec<String> {
    let mut queries = Vec::new();
    for raw in [
        Some(media.title.as_str()),
        media.original_title.as_deref(),
        media.aliases.first().map(String::as_str),
    ]
    .into_iter()
    .flatten()
    {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !queries.iter().any(|q: &String| q == trimmed) {
            queries.push(trimmed.to_string());
        }
    }
    queries
}

fn normalize_title(raw: &str) -> String {
    raw.trim().to_lowercase()
}

fn normalized_media_names(media: &Media) -> Vec<String> {
    let mut out = Vec::new();
    for raw in [Some(media.title.as_str()), media.original_title.as_deref()]
        .into_iter()
        .flatten()
        .chain(media.aliases.iter().map(String::as_str))
    {
        let norm = normalize_title(raw);
        if !norm.is_empty() && !out.contains(&norm) {
            out.push(norm);
        }
    }
    out
}

fn collect_matching_hits(
    tmdb: &dyn Catalog,
    kind: MediaKind,
    queries: &[String],
    douban_media: &Media,
    douban_names: &[String],
) -> Vec<CatalogHit> {
    let mut seen = HashSet::new();
    let mut matched = Vec::new();
    for query in queries {
        let hits = match kind {
            MediaKind::Tv => tmdb.search_tv(query),
            MediaKind::Movie | MediaKind::Video => tmdb.search_movie(query),
        }
        .unwrap_or_default();
        for hit in hits {
            let Some(tmdb_id) = hit.media.tmdb_id.clone().filter(|s| !s.is_empty()) else {
                continue;
            };
            let hit_names = normalized_media_names(&hit.media);
            let title_ok = hit_names
                .iter()
                .any(|hn| douban_names.iter().any(|dn| dn == hn));
            let year_ok = match (douban_media.year, hit.media.year) {
                (Some(dy), Some(hy)) => hy.abs_diff(dy) <= 1,
                _ => true,
            };
            if title_ok && year_ok && seen.insert((hit.media.kind, tmdb_id)) {
                matched.push(hit);
            }
        }
        if !matched.is_empty() {
            break;
        }
    }
    matched
}

fn candidate_rank(douban: &Media, hit: &CatalogHit) -> (bool, bool, bool) {
    let same_kind = hit.media.kind == douban.kind;
    let exact_year = matches!((douban.year, hit.media.year), (Some(dy), Some(hy)) if dy == hy);
    let primary = normalize_title(&douban.title);
    let primary_match = normalize_title(&hit.media.title) == primary
        || hit
            .media
            .original_title
            .as_deref()
            .is_some_and(|ot| normalize_title(ot) == primary);
    (same_kind, exact_year, primary_match)
}

fn finalize_anchored_media(tmdb: &dyn Catalog, douban_media: &Media, winner: CatalogHit) -> Media {
    let tmdb_id = winner.media.tmdb_id.clone().unwrap_or_default();
    let mut resolved = tmdb
        .details(winner.media.kind, &tmdb_id)
        .ok()
        .flatten()
        .unwrap_or(winner.media);
    resolved.tmdb_id = Some(tmdb_id);
    resolved.douban_id = douban_media.douban_id.clone();
    if resolved.original_title.is_none() {
        resolved.original_title = douban_media
            .original_title
            .clone()
            .filter(|ot| ot != &resolved.title);
    }
    let title = resolved.title.clone();
    merge_aliases(&mut resolved.aliases, &douban_media.aliases, &title);
    resolved
}

fn merge_aliases(target: &mut Vec<String>, extra: &[String], primary_title: &str) {
    for alias in extra {
        let trimmed = alias.trim();
        if !trimmed.is_empty()
            && trimmed != primary_title
            && !target.iter().any(|existing| existing == trimmed)
        {
            target.push(trimmed.to_string());
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

fn resolve_preview(state: &ApiState, title: &TitleRef) -> PreviewOutcome {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    match title {
        TitleRef::Tmdb { kind, id } => match catalog.details(*kind, id).unwrap_or(None) {
            Some(media) => PreviewOutcome::Ready(media),
            None => PreviewOutcome::NotFound,
        },
        TitleRef::Douban(id) => {
            if let Some(local) = local_douban_media(state, id) {
                return PreviewOutcome::Ready(local);
            }
            let Some(douban_media) = fetch_douban_media(catalog, id).unwrap_or(None) else {
                return PreviewOutcome::NotFound;
            };
            match anchor_douban_to_tmdb(catalog, &douban_media) {
                TmdbAnchorOutcome::Unique(anchored) => PreviewOutcome::Ready(anchored),
                TmdbAnchorOutcome::Ambiguous(candidates) => {
                    PreviewOutcome::Ambiguous(candidates.into_iter().map(candidate_json).collect())
                }
                TmdbAnchorOutcome::None => PreviewOutcome::Ready(douban_media),
            }
        }
    }
}

fn candidate_json(hit: CatalogHit) -> Value {
    let tmdb_id = hit.media.tmdb_id.clone().unwrap_or_default();
    let poster_url = hit.poster_path.as_deref().and_then(|p| {
        if p.starts_with("http://") || p.starts_with("https://") {
            Some(p.to_string())
        } else if p.starts_with('/') {
            Some(format!("{TMDB_POSTER}{p}"))
        } else {
            None
        }
    });
    json!({
        "title_ref": format!("tmdb:{}:{tmdb_id}", hit.media.kind.as_str()),
        "tmdb_id": tmdb_id,
        "kind": hit.media.kind.as_str(),
        "title": hit.media.title,
        "original_title": hit.media.original_title,
        "year": hit.media.year,
        "poster_url": poster_url,
        "overview": hit.overview,
    })
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
    let resolved = match resolve_preview(state, &title) {
        PreviewOutcome::Ready(media) => media,
        PreviewOutcome::Ambiguous(candidates) => {
            return ok(json!({
                "status": "ambiguous", "media": Value::Null, "seasons": [],
                "existing_subscription_id": Value::Null, "movie_owned": false,
                "suggested_seasons": [], "candidates": candidates,
            }))
            .into_response();
        }
        PreviewOutcome::NotFound => {
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

type PreviewFacts = (Media, Option<String>, HashMap<u32, usize>, bool);

fn preview_facts(
    state: &ApiState,
    resolved: Media,
    current_user_id: Option<domain::UserId>,
    season: Option<u32>,
) -> Option<PreviewFacts> {
    super::store_wait::with_store_read(
        state,
        &[store::Db::Library, store::Db::App, store::Db::Subscribe],
        |store| -> Option<PreviewFacts> {
            let visible =
                crate::visible_scope::VisibleLibraries::load(store, current_user_id).ok()?;
            let media = resolved
                .tmdb_id
                .as_deref()
                .and_then(|id| {
                    store
                        .get_media_by_alias_kind("tmdb", id, Some(resolved.kind))
                        .ok()
                        .flatten()
                })
                .or_else(|| {
                    resolved.douban_id.as_deref().and_then(|id| {
                        store
                            .get_media_by_alias_kind("douban", id, Some(resolved.kind))
                            .ok()
                            .flatten()
                    })
                })
                .map(|mut stored| {
                    if stored.douban_id.is_none() {
                        stored.douban_id = resolved.douban_id.clone();
                    }
                    if stored.tmdb_id.is_none() {
                        stored.tmdb_id = resolved.tmdb_id.clone();
                    }
                    stored
                })
                .unwrap_or(resolved);
            let media_id = media.id;
            let existing = store
                .list_all_subscribes()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| current_user_id.is_none_or(|uid| s.user_id == uid))
                .filter(|s| s.media_id == media_id)
                .find(|s| match (&s.coverage, season) {
                    (domain::Coverage::Tv { season, .. }, Some(target_season)) => {
                        *season == target_season
                    }
                    _ => true,
                })
                .map(|s| s.id.to_string());

            let ledger: Vec<domain::FileCoverage> = store
                .file_coverage_for_media(media_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|row| {
                    row.file.publication == domain::FilePublication::Published
                        && row.file.presence != domain::FilePresence::Missing
                        && visible.get(&row.file.library_id).is_some()
                })
                .collect();
            let (owned_seasons, has_owned) = owned_episode_counts(&ledger, media_id);
            let movie_owned = media.kind == MediaKind::Movie && has_owned;
            Some((media, existing, owned_seasons, movie_owned))
        },
    )
    .ok()
    .flatten()
}

fn preview_seasons(
    state: &ApiState,
    media: &Media,
    owned_seasons: &HashMap<u32, usize>,
) -> Vec<Value> {
    if media.kind != MediaKind::Tv {
        return Vec::new();
    }
    if let Some(tmdb_id) = media.tmdb_id.as_deref() {
        let rows = state.catalog.tv_seasons(tmdb_id).unwrap_or_default();
        tracing::debug!(
            tmdb_id = %tmdb_id,
            rows_count = rows.len(),
            "title-preview 正在拉取 TV seasons"
        );
        let today = today_rfc3339();
        let current_year = today.get(..4).unwrap_or("9999");
        return rows
            .iter()
            .map(|season| {
                let aired = season
                    .air_date
                    .as_deref()
                    .is_some_and(|date| date <= today.as_str());
                let aired_count = if aired && season.season_number > 0 {
                    season_aired_episode_count(
                        state.catalog.as_ref(),
                        tmdb_id,
                        season,
                        &today,
                        current_year,
                        rows.len(),
                    )
                } else if aired {
                    season.episode_count.unwrap_or(0)
                } else {
                    0
                };
                let owned = owned_seasons
                    .get(&season.season_number)
                    .copied()
                    .unwrap_or(0);
                json!({
                    "season_number": season.season_number,
                    "name": season.name,
                    "air_date": season.air_date,
                    "episode_count": season.episode_count,
                    "aired_count": aired_count,
                    "owned_count": owned,
                })
            })
            .collect();
    }
    let episodes_count_opt = media
        .douban_id
        .as_deref()
        .and_then(|did| state.catalog.douban_subject_with_credits(did).ok())
        .and_then(|pair| rexxar_episodes_count(&pair.subject));
    let owned = owned_seasons.get(&1).copied().unwrap_or(0);
    vec![json!({
        "season_number": 1,
        "name": "第 1 季",
        "air_date": Value::Null,
        "episode_count": episodes_count_opt,
        "aired_count": episodes_count_opt.unwrap_or(0),
        "owned_count": owned,
    })]
}

fn season_aired_episode_count(
    catalog: &dyn Catalog,
    tmdb_id: &str,
    season: &media::TvSeason,
    today: &str,
    current_year: &str,
    total_seasons: usize,
) -> u32 {
    let cached = catalog
        .cached_season_episodes(tmdb_id, season.season_number)
        .ok()
        .flatten();
    let episodes = match cached {
        Some(rows) if !rows.is_empty() => Some(rows),
        _ => {
            let is_recent_season = season
                .air_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .is_some_and(|yr| yr >= current_year || total_seasons <= 2);
            if is_recent_season {
                catalog
                    .season_episodes(tmdb_id, season.season_number)
                    .ok()
                    .filter(|rows| !rows.is_empty())
            } else {
                None
            }
        }
    };
    match episodes {
        Some(rows) => rows
            .iter()
            .filter(|ep| ep.air_date.as_deref().is_some_and(|d| d <= today))
            .count() as u32,
        None => season.episode_count.unwrap_or(0),
    }
}

fn owned_episode_counts(
    coverage: &[domain::FileCoverage],
    media_id: domain::MediaId,
) -> (HashMap<u32, usize>, bool) {
    let mut distinct_episodes: HashMap<u32, HashSet<u32>> = HashMap::new();
    let mut has_owned = false;
    for row in coverage.iter().filter(|row| row.file.media_id == media_id) {
        has_owned = true;
        for unit in &row.units {
            if let Some((season, episode)) = unit.episode_coordinates() {
                distinct_episodes.entry(season).or_default().insert(episode);
            }
        }
    }
    let owned_seasons = distinct_episodes
        .into_iter()
        .map(|(season, episodes)| (season, episodes.len()))
        .collect();
    (owned_seasons, has_owned)
}

fn today_rfc3339() -> String {
    let s = crate::store::now_rfc3339();
    s.split('T').next().unwrap_or(&s).to_string()
}

pub(crate) fn media_identity(media: &Media) -> Value {
    json!({
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "aliases": media.aliases,
        "tmdb_id": media.tmdb_id,
        "douban_id": media.douban_id,
        "tvdb_id": media.tvdb_id,
        "bangumi_id": media.bangumi_id,
        "anilist_id": media.anilist_id,
    })
}

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
