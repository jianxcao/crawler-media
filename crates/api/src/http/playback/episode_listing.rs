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
    let (media, visible_cloned, unit_states) = {
        let store = state.store.lock();
        let Some(media_id) = resolve_media_id(&id) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let all_coverages = store.file_coverage_for_media(media.id).unwrap_or_default();
        let visible_covs = visible_coverages_for_media(&store, &media, &all_coverages, user_id);
        if visible_covs.is_empty() {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        }
        let visible_cloned: Vec<domain::FileCoverage> = visible_covs.into_iter().cloned().collect();
        let mut unit_states = std::collections::HashMap::new();
        if let Some(uid) = user_id {
            for cov in &visible_cloned {
                for unit in &cov.units {
                    if unit.is_whole() {
                        continue;
                    }
                    let (s, e) = (unit.season.max(0) as u32, unit.episode.max(0) as u32);
                    if let Ok(st) = store.unit_state(uid, media.id, unit.season, unit.episode) {
                        unit_states.insert((s, e), st);
                    }
                }
            }
        }
        (media, visible_cloned, unit_states)
    };

    let files = state.browse_files.clone();
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

fn render_rows(
    id: &str,
    visible_cloned: &[domain::FileCoverage],
    season_number: Option<u32>,
    unit_states: &std::collections::HashMap<(u32, u32), Option<UnitState>>,
    files: &dyn crate::browse_file_read::BrowseFileRead,
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
            if let Some(target) = season_number {
                if season_num != target {
                    continue;
                }
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
