use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::http::ok;
use crate::management::ApiState;

use parking_lot::Mutex;
use std::sync::LazyLock;

/// A local up-next plan, with a separate key for background name enrichment.
#[derive(Clone)]
struct UpNextPlan {
    kind: domain::MediaKind,
    tmdb_id: Option<String>,
    season: u32,
    next_episode: i32,
}

type EpisodeTitleKey = (String, u32, i32);
type SeasonFlightKey = (String, String, u32);
static TITLE_FLIGHTS: LazyLock<Mutex<HashSet<SeasonFlightKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

struct SeasonFlightLease(SeasonFlightKey);

impl Drop for SeasonFlightLease {
    fn drop(&mut self) {
        TITLE_FLIGHTS.lock().remove(&self.0);
    }
}

/// 后台季名补全的尝试时间：键为 `(tmdb_id, season)`。
///
/// 单飞集合只能去重**并发**请求；没有冷却时，一个 TMDB 解析不出的季会在每个
/// up-next 请求上重新排一次补全，而每次补全都会占用一个共享 `search_work`
/// permit 直到整个 TMDB 抓取结束，把交互式搜索挤成 503 `search.busy`。
static TITLE_ENRICH_ATTEMPTS: LazyLock<Mutex<HashMap<(String, u32), std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 同一季的补全尝试间隔；期间不再重新排程（有界，不随请求数增长）。
const TITLE_ENRICH_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(600);
/// 冷却表的硬上限；超过时先清过期项，仍满时拒绝新预约。
const TITLE_ENRICH_ATTEMPTS_CAP: usize = 1024;

/// 记录一次补全尝试；冷却期内返回 `false`（调用方不再排程）。
fn reserve_enrichment_attempt(tmdb_id: &str, season: u32) -> bool {
    let mut attempts = TITLE_ENRICH_ATTEMPTS.lock();
    let now = std::time::Instant::now();
    let key = (tmdb_id.to_owned(), season);
    if attempts
        .get(&key)
        .is_some_and(|attempted| now.duration_since(*attempted) < TITLE_ENRICH_COOLDOWN)
    {
        return false;
    }
    if attempts.len() >= TITLE_ENRICH_ATTEMPTS_CAP {
        attempts.retain(|_, attempted| now.duration_since(*attempted) < TITLE_ENRICH_COOLDOWN);
        if attempts.len() >= TITLE_ENRICH_ATTEMPTS_CAP {
            return false;
        }
    }
    attempts.insert(key, now);
    true
}
mod read;

/// Select continuation references in one read snapshot, then render the returned page.
pub(crate) async fn up_next(
    State(state): State<ApiState>,
    axum::Extension(user): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    for attempt in 0..2 {
        let captured = match super::super::store_wait::store_read(&state, move |store| {
            read::capture(store, user, limit)
        })
        .await
        {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                tracing::error!(%error, %user, "up-next selection failed");
                return store_failed();
            }
            Err(error) => return error.response(),
        };
        let snapshot = captured.snapshot;
        let files = state.file_stats.clone();
        let cold_state = state.clone();
        let rendered = match state
            .http_work
            .run(std::time::Duration::from_secs(30), move || {
                render_page(&cold_state, files.as_ref(), captured.candidates)
            })
            .await
        {
            Ok(items) => items,
            Err(error) => return blocking_failed(error),
        };
        let validation = match super::super::store_wait::store_read(&state, move |store| {
            read::validate(store, user, &snapshot)
        })
        .await
        {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                tracing::error!(%error, %user, "up-next validation failed");
                return store_failed();
            }
            Err(error) => return error.response(),
        };
        if matches!(validation, store::PageValidation::Stale { .. }) && attempt == 0 {
            continue;
        }
        let gone: HashSet<_> = match validation {
            store::PageValidation::Fresh => HashSet::new(),
            store::PageValidation::Stale { gone } => gone.into_iter().collect(),
        };
        let items: Vec<_> = rendered
            .into_iter()
            .filter(|(sources, _)| !sources.iter().any(|source| gone.contains(source)))
            .map(|(_, item)| item)
            .collect();
        return ok(json!({"items": items})).into_response();
    }
    store_failed()
}

fn render_page(
    state: &ApiState,
    files: &dyn crate::file_stat_read::FileStatRead,
    candidates: Vec<UpNextCandidate>,
) -> Vec<(Vec<domain::LedgerId>, serde_json::Value)> {
    let plans = candidates
        .iter()
        .map(|cand| UpNextPlan {
            kind: cand.media.kind,
            tmdb_id: cand.media.tmdb_id.clone(),
            season: cand.plan_season,
            next_episode: cand.plan_next_episode,
        })
        .collect::<Vec<_>>();
    let titles = cached_episode_titles(state, &plans);
    candidates
        .into_iter()
        .map(|mut cand| {
            let sources = cand
                .next_row
                .iter()
                .chain(cand.preferred_coverage.iter())
                .map(|row| row.file.file_id)
                .collect();
            cand.episode_title = cand
                .media
                .tmdb_id
                .as_ref()
                .and_then(|id| titles.get(&(id.clone(), cand.plan_season, cand.plan_next_episode)))
                .cloned()
                .flatten();
            (sources, render_item(state, files, cand))
        })
        .collect()
}

/// A local cache hit may supply the episode name; misses are refreshed asynchronously.
fn cached_episode_titles(
    state: &ApiState,
    plans: &[UpNextPlan],
) -> HashMap<EpisodeTitleKey, Option<String>> {
    let mut titles = HashMap::new();
    for plan in plans {
        if plan.kind != domain::MediaKind::Tv {
            continue;
        }
        let Some(tmdb_id) = plan.tmdb_id.as_deref() else {
            continue;
        };
        match state.catalog.cached_season_details(tmdb_id, plan.season) {
            Ok(Some(details)) => {
                let title = details
                    .into_iter()
                    .find(|episode| episode.episode_number == plan.next_episode as u32)
                    .and_then(|episode| episode.name);
                if title.is_none() {
                    schedule_season_enrichment(state, tmdb_id, plan.season);
                }
                titles.insert((tmdb_id.to_owned(), plan.season, plan.next_episode), title);
            }
            Ok(None) => schedule_season_enrichment(state, tmdb_id, plan.season),
            Err(error) => {
                tracing::warn!(
                    kind = "tv",
                    season = plan.season,
                    error_type = error_type(&error),
                    "up-next cached season lookup failed"
                );
                schedule_season_enrichment(state, tmdb_id, plan.season);
            }
        }
    }
    titles
}

struct UpNextCandidate {
    media: domain::Media,
    library_id: String,
    preferred_coverage: Option<domain::FileCoverage>,
    next_row: Option<domain::FileCoverage>,
    plan_season: u32,
    plan_next_episode: i32,
    episode_title: Option<String>,
    position: i64,
    duration: Option<i64>,
    progress: Option<i64>,
    advanced: bool,
    last_played_at: String,
}

fn render_item(
    state: &ApiState,
    files: &dyn crate::file_stat_read::FileStatRead,
    cand: UpNextCandidate,
) -> serde_json::Value {
    use crate::http::library::artwork_url;
    let preferred = cand.preferred_coverage.as_ref();
    let poster = preferred
        .filter(|row| crate::http::library::poster_path_with(&row.file, files).is_some())
        .map(|row| artwork_url("posters", row.file.file_id));
    let observed_backdrop = preferred
        .filter(|row| crate::http::library::backdrop_path_with(&row.file, files).is_some())
        .map(|row| artwork_url("fanart", row.file.file_id));
    if observed_backdrop.is_none()
        && let Some(preferred) = preferred
    {
        state.backdrop_enrichment.schedule(
            state.clone(),
            cand.media.clone(),
            preferred.file.path.clone(),
        );
    }
    let fanart_fallback = preferred.map(|row| artwork_url("fanart", row.file.file_id));
    let still = cand
        .next_row
        .as_ref()
        .and_then(|row| {
            crate::episode_still::existing_with(row.file.path.as_path(), files)
                .map(|_| artwork_url("stills", row.file.file_id))
        })
        .or_else(|| observed_backdrop.clone())
        .or_else(|| fanart_fallback.clone());
    let backdrop = observed_backdrop.or(fanart_fallback);
    let episode_title = if cand.media.kind == domain::MediaKind::Tv {
        // 集名从**该集自己的** `.nfo` 读（`path.with_extension("nfo")`），再回落缓存集名。
        cand.next_row
            .as_ref()
            .and_then(|row| files.read_nfo(&row.file.path.with_extension("nfo")))
            .and_then(|nfo| nfo.title)
            .or(cand.episode_title)
    } else {
        // 电影/视频的 `next_row` 就是媒体自身，不是集行：不做 NFO 标题回落。
        cand.episode_title
    };
    json!({
        "media_item_id": cand.media.id.to_string(),
        "library_id": cand.library_id,
        "kind": cand.media.kind.as_str(),
        "title": cand.media.title,
        "year": cand.media.year,
        "poster_url": poster,
        "poster_aspect": 0.667,
        "backdrop_url": backdrop,
        "episode_still_url": still,
        "season_number": cand.plan_season,
        "episode_number": cand.plan_next_episode,
        "episode_title": episode_title,
        "unwatched_ahead_count": 0,
        "position_ms": cand.position,
        "duration_ms": cand.duration,
        "progress_percent": cand.progress,
        "advanced": cand.advanced,
        "last_played_at": cand.last_played_at,
    })
}

fn blocking_failed(error: crate::blocking_budget::BlockingError) -> Response {
    use axum::http::StatusCode;
    match error {
        crate::blocking_budget::BlockingError::Busy => super::super::store_wait::busy_response(),
        crate::blocking_budget::BlockingError::Timeout => crate::http::err(
            StatusCode::GATEWAY_TIMEOUT,
            "up_next.timeout",
            "待播列表加载超时",
        ),
        crate::blocking_budget::BlockingError::Worker(msg) => {
            crate::http::err(StatusCode::INTERNAL_SERVER_ERROR, "up_next.worker", &msg)
        }
    }
}

fn millis(unix_secs: i64) -> String {
    (unix_secs as i128 * 1000).to_string()
}
fn schedule_season_enrichment(state: &ApiState, tmdb_id: &str, season: u32) {
    let key = ("tv".to_owned(), tmdb_id.to_owned(), season);
    if !reserve_enrichment_attempt(tmdb_id, season) {
        return;
    }
    {
        let mut flights = TITLE_FLIGHTS.lock();
        if !flights.insert(key.clone()) {
            return;
        }
    }
    let catalog = state.catalog.clone();
    let search_work = state.search_work.clone();
    tokio::spawn(async move {
        let worker_key = key.clone();
        let lease = SeasonFlightLease(key);
        let result = search_work
            .run_to_completion(move || {
                let _lease = lease;
                let result = catalog.season_details(&worker_key.1, worker_key.2);
                if let Err(error) = &result {
                    tracing::warn!(kind = "tv", tmdb_id = %worker_key.1, season = worker_key.2,
                    error_type = error_type(error), "up-next cached season lookup failed");
                }
                result
            })
            .await;
        if result.is_err() {
            tracing::debug!(
                kind = "tv",
                season,
                error_type = "worker",
                "up-next enrichment worker unavailable"
            );
        }
    });
}

fn error_type(_error: &str) -> &'static str {
    "catalog"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表满时不得为了收下新键而丢弃**仍在冷却期内**的预约。
    #[test]
    fn full_enrichment_table_preserves_live_reservations() {
        for season in 0..TITLE_ENRICH_ATTEMPTS_CAP as u32 {
            assert!(
                reserve_enrichment_attempt("capacity-regression", season),
                "reservation {season} must be accepted below the cap"
            );
        }
        assert!(
            !reserve_enrichment_attempt("capacity-regression-overflow", 0),
            "a full table must reject the new reservation instead of clearing live ones"
        );
        assert!(
            !reserve_enrichment_attempt("capacity-regression", 0),
            "an unexpired reservation must survive a full table"
        );
    }
}

fn store_failed() -> Response {
    crate::http::err(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "up_next.store",
        "接下来看读取失败",
    )
}
