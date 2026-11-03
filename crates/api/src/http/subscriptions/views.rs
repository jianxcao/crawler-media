use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Coverage, Media, Subscribe, UserId};
use serde_json::{Value, json};

use crate::http::{err, ok_list};
use crate::management::ApiState;
use release;

pub(super) fn media_json(media: &Media, poster_url: Option<String>) -> Value {
    json!({
        "id": media.id.to_string(),
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "tmdb_id": media.tmdb_id,
        "douban_id": media.douban_id,
        "tvdb_id": media.tvdb_id,
        "bangumi_id": media.bangumi_id,
        "anilist_id": media.anilist_id,
        "poster_url": poster_url,
    })
}

pub(crate) fn coverage_json(coverage: &Coverage) -> Value {
    match coverage {
        Coverage::Movie => json!({ "kind": "movie" }),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => json!({
            "kind": "tv",
            "season": season,
            "episode_from": episode_from,
            "episode_to": episode_to,
        }),
    }
}

pub(crate) fn render_subscription_snapshot(
    snapshot: &super::snapshot::SubscriptionViewSnapshot,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Value {
    let subscribe = &snapshot.subscribe;
    let media = &snapshot.media;
    let total = snapshot.total;
    let imported = snapshot.imported;
    let pending = &snapshot.pending;
    let facts = &snapshot.facts;

    let grabbing = count_pipeline_units(subscribe, pending, facts);
    let downloaded = (imported + grabbing).min(total);

    let created_at = &snapshot.created_at;
    let updated_at = &snapshot.updated_at;

    let poster_choice = super::poster_view::determine_poster_from_candidate(
        media,
        snapshot
            .library_poster_candidate
            .as_ref()
            .filter(|(_, path)| files.is_file(path))
            .map(|(file, _)| *file),
    );
    let poster_url = super::poster_view::poster_url(poster_choice);

    json!({
        "id": subscribe.id.to_string(),
        "media": media_json(media, poster_url),
        "user_id": subscribe.user_id.to_string(),
        "coverage": coverage_json(&subscribe.coverage),
        "fetch_mode": subscribe.fetch_mode.as_str(),
        "filter_id": subscribe.filter_id.to_string(),
        "wash_cut": subscribe.wash_cut,
        "keep_old_versions": subscribe.keep_old_versions,
        "wash_cut_filter_id": subscribe.wash_cut_filter_id.map(|id| id.to_string()),
        "full_season_pack": subscribe.full_season_pack,
        "downloader_id": subscribe.downloader_id.map(|id| id.to_string()),
        "library_id": subscribe.library_id.map(|id| id.to_string()),
        "target_library_status": snapshot.target_library_status,
        "target_library_error": snapshot.target_library_error,
        "tracking_state": subscribe.tracking_state,
        "follow_future": subscribe.follow_future,
        "search_interval_secs": subscribe.search_interval_secs,
        "progress": {
            "total": total,
            "imported": imported,
            "missing": (total - downloaded).max(0),
            "grabbing": grabbing,
            "downloaded": downloaded,
        },
        "created_at": created_at,
        "updated_at": updated_at,
    })
}

pub(crate) fn count_pipeline_units(
    subscribe: &Subscribe,
    pending: &[(i32, crate::store::PendingDownload)],
    facts: &domain::SubscribeFacts,
) -> i64 {
    if pending.is_empty() {
        return 0;
    }
    match subscribe.coverage {
        Coverage::Movie => {
            if facts.movie().is_none() {
                1
            } else {
                0
            }
        }
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let mut covered = std::collections::HashSet::new();
            for (_, p) in pending {
                let r = release::parse(&p.torrent.title);
                if r.season == Some(season) {
                    for (_, ep) in r.covered_episodes() {
                        let in_range =
                            ep >= episode_from && episode_to.map(|t| ep <= t).unwrap_or(true);
                        if in_range && facts.get(Some(season), Some(ep)).is_none() {
                            covered.insert(ep);
                        }
                    }
                }
            }
            // 若为全季包等未标明具体每集的种子，且有 pending，则兜底至少为 1（若仍有缺口）
            if covered.is_empty() {
                pending.len() as i64
            } else {
                covered.len() as i64
            }
        }
    }
}

pub(crate) async fn list_subscriptions(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
) -> Response {
    let snapshots = match crate::http::store_wait::store_read(
        &state,
        &[
            store::Db::Library,
            store::Db::App,
            store::Db::Subscribe,
            store::Db::Cache,
        ],
        move |store| super::snapshot::load_visible_subscription_views(store, user_id),
    )
    .await
    {
        Ok(Ok(snapshots)) => snapshots,
        Ok(Err(error)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
        Err(error) => return error.response(),
    };
    let files = state.file_stats.clone();
    match state
        .http_work
        .run(crate::http::library::BROWSE_DEADLINE, move || {
            snapshots
                .iter()
                .map(|snapshot| render_subscription_snapshot(snapshot, files.as_ref()))
                .collect()
        })
        .await
    {
        Ok(rows) => ok_list(rows).into_response(),
        Err(error) => crate::http::library::browse_work_error(error),
    }
}
