//! Playback episode list snapshots and lock-free artwork rendering.
use super::*;

pub(crate) async fn episodes(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let season_number = match query.get("season_number") {
        Some(s) => match s.parse::<u32>() {
            Ok(num) => Some(num),
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid",
                    "season_number 无效",
                );
            }
        },
        None => None,
    };
    // Review L2: episode snapshots are a ledger read; take the lock off-executor.
    let id_for_read = id.clone();
    let (media, visible_cloned, unit_states) = match store_read(&state, move |store| {
        let is_admin = match user_id {
            Some(user) => store.user_role(user).map_err(episode_store_error)? == "admin",
            None => false,
        };
        store
            .library_read_snapshot(|store| {
                load_episode_snapshot(store, &id_for_read, user_id, season_number, is_admin)
            })
            .map_err(episode_store_error)?
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(response)) => return response,
        Err(error) => return error.response(),
    };

    let files = state.file_stats.clone();
    let id_clone = id.clone();
    let rows_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            render_rows(
                &id_clone,
                &visible_cloned,
                season_number,
                &unit_states,
                files.as_ref(),
            )
        })
        .await;

    let rows = match rows_res {
        Ok(rows) => rows,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return err(
                StatusCode::GATEWAY_TIMEOUT,
                "playback.timeout",
                "加载集列表超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(message)) => {
            tracing::error!(%message, media_id = %media.id, "读取播放集列表 blocking worker 失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime.worker",
                "加载集列表失败",
            );
        }
    };
    ok(json!({ "season_number": season_number, "episodes": rows })).into_response()
}

type EpisodeSnapshot = (
    domain::Media,
    Vec<domain::FileCoverage>,
    std::collections::HashMap<(u32, u32), Option<UnitState>>,
);

fn episode_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "读取分集失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "读取分集失败",
    )
}

fn load_episode_snapshot(
    store: &store::LibraryConnection<'_>,
    id: &str,
    user_id: Option<domain::UserId>,
    season: Option<u32>,
    is_admin: bool,
) -> Result<EpisodeSnapshot, Response> {
    let missing = || err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    let media_id = resolve_media_id(id).ok_or_else(missing)?;
    let media = store
        .get_media(media_id)
        .map_err(episode_store_error)?
        .ok_or_else(missing)?;
    let visible = crate::visible_scope::VisibleLibraries::load_on(store, user_id, is_admin)
        .map_err(episode_store_error)?;
    let mut coverage = Vec::new();
    let mut has_file = false;
    for library in visible.ids() {
        has_file |= store
            .has_file_in_library(library, media.id)
            .map_err(episode_store_error)?;
        let rows = match season {
            Some(season) => store.file_coverage_in_library_season(library, media.id, season),
            None => store.file_coverage_in_library(
                library,
                Some(media.id),
                store::FileReadScope::AllRecorded,
            ),
        }
        .map_err(episode_store_error)?;
        coverage.extend(rows);
    }
    if !has_file {
        return Err(missing());
    }
    let units = season_units(&coverage, season);
    let states = match user_id {
        Some(user) => units
            .iter()
            .zip(
                store
                    .effective_unit_states(user, media.id, &units)
                    .map_err(episode_store_error)?,
            )
            .map(|(unit, state)| {
                (
                    (unit.season.max(0) as u32, unit.episode.max(0) as u32),
                    state,
                )
            })
            .collect(),
        None => std::collections::HashMap::new(),
    };
    Ok((media, coverage, states))
}

/// Deduped episode units in the order they are rendered.
///
/// The requested season bounds the work: a season view must not pay for every
/// episode the Media owns, and a whole-item unit carries no episode coordinates to
/// resolve.
fn season_units(
    coverages: &[domain::FileCoverage],
    season_number: Option<u32>,
) -> Vec<domain::UnitKey> {
    let mut seen = std::collections::HashSet::new();
    let mut units = Vec::new();
    for cov in coverages {
        for unit in &cov.units {
            if unit.is_whole() {
                continue;
            }
            if season_number.is_some_and(|target| unit.season.max(0) as u32 != target) {
                continue;
            }
            if seen.insert(*unit) {
                units.push(*unit);
            }
        }
    }
    units
}

fn render_rows(
    id: &str,
    visible_cloned: &[domain::FileCoverage],
    season_number: Option<u32>,
    unit_states: &std::collections::HashMap<(u32, u32), Option<UnitState>>,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Vec<Value> {
    let mut grouped: std::collections::HashMap<
        (u32, u32),
        (Vec<String>, String, Option<String>, Option<String>),
    > = std::collections::HashMap::new();

    for cov in visible_cloned {
        for unit in &cov.units {
            if unit.is_whole() {
                continue;
            }
            let season_num = unit.season.max(0) as u32;
            let ep_num = unit.episode.max(0) as u32;
            if let Some(target) = season_number
                && season_num != target
            {
                continue;
            }

            let file_path = &cov.file.path;
            let file_name = file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();

            let local_still = crate::episode_still::existing_with(file_path, files)
                .map(|_| crate::http::library::artwork_url("stills", cov.file.file_id));
            let backdrop = if !unit.is_whole()
                && crate::http::library::backdrop_path_with(&cov.file, files).is_some()
            {
                Some(crate::http::library::artwork_url(
                    "fanart",
                    cov.file.file_id,
                ))
            } else {
                None
            };

            let entry = grouped
                .entry((season_num, ep_num))
                .or_insert_with(|| (Vec::new(), file_name, local_still, backdrop));
            entry.0.push(cov.file.file_id.to_string());
        }
    }

    let mut keys: Vec<(u32, u32)> = grouped.keys().copied().collect();
    keys.sort();

    let episode_rows: Vec<episodes::EpisodeRow> = keys
        .into_iter()
        .map(|(season, episode)| {
            let (file_ids, file_name, local_still, backdrop) =
                grouped.remove(&(season, episode)).unwrap();

            let (position_ms, played, progress_percent) = match unit_states
                .get(&(season, episode))
                .and_then(|state| state.as_ref())
            {
                Some(ps) => {
                    let percent = if !ps.played {
                        ps.duration_ms.and_then(|d| {
                            if d > 0 {
                                Some(((ps.position_ms * 100) / d).clamp(0, 100))
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    };
                    (ps.position_ms, ps.played, percent)
                }
                None => (0, false, None),
            };

            episodes::EpisodeRow {
                season,
                episode,
                file_ids,
                file_name,
                local_still,
                tmdb_still: None,
                backdrop,
                name: None,
                overview: None,
                position_ms,
                played,
                progress_percent,
            }
        })
        .collect();
    episodes::format_episodes_json(&episode_rows, id)
}
