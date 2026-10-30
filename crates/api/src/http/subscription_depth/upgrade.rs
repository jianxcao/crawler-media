use super::*;
use crate::http::subscriptions::{resolve_subscribe, resolve_subscribe_on};

pub(crate) async fn run_upgrade(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let blocking = state.clone();
    crate::http::subscriptions::guarded_blocking(&blocking, move || {
        upgrade_blocking(state, user_id, id, body)
    })
    .await
}

struct Snapshot {
    subscribe: Subscribe,
    media: domain::Media,
    facts: subscribe::SubscribeFacts,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    history: std::collections::HashMap<(Option<u32>, Option<u32>), crate::store::WantedHistory>,
    target_score: i32,
    target_label: String,
}

fn capture(
    state: &ApiState,
    id: &str,
    user: domain::UserId,
    body: axum::body::Bytes,
) -> Result<Snapshot, Response> {
    crate::http::store_wait::with_foreground_store(state, |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        let (mut subscribe, media) = resolve_subscribe_on(store, id, user)?;
        let (input, has_body): (RunUpgradeInput, bool) =
            if body.is_empty() || body.as_ref() == b"null" {
                (RunUpgradeInput::default(), false)
            } else {
                (
                    serde_json::from_slice(&body).map_err(|error| {
                        err(
                            StatusCode::BAD_REQUEST,
                            "subscription.invalid",
                            &error.to_string(),
                        )
                    })?,
                    true,
                )
            };
        let mut dirty = false;
        if let Some(filter) = input
            .filter_id
            .and_then(|id| id.parse::<domain::FilterId>().ok())
            && store.get_filter(filter).map_err(store_error)?.is_some()
        {
            subscribe.wash_cut_filter_id = Some(filter);
            dirty = true;
        }
        if has_body && input.wash_cut.unwrap_or(true) && !subscribe.wash_cut {
            subscribe.wash_cut = true;
            dirty = true;
        }
        if dirty {
            store.update_subscribe(&subscribe).map_err(store_error)?;
        }
        let (target_score, target_label) = wash_target(store, &subscribe);
        Ok(Snapshot {
            facts: store
                .load_subscribe_facts(subscribe.id)
                .map_err(store_error)?,
            pending: store.load_pending(subscribe.id).map_err(store_error)?,
            history: store
                .load_wanted_history(subscribe.id)
                .map_err(store_error)?,
            subscribe,
            media,
            target_score,
            target_label,
        })
    })?
}

fn upgrade_blocking(
    state: ApiState,
    user: domain::UserId,
    id: String,
    body: axum::body::Bytes,
) -> Response {
    let (initial, _) = match resolve_subscribe(&state, &id, user) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let guard = state.subscribe_guard(initial.id);
    let Some(_lease) = guard.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT) else {
        return crate::http::store_wait::busy_response();
    };
    if state.subscribe_is_deleting(initial.id) {
        return err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除，无法修改",
        );
    }
    let snapshot = match capture(&state, &id, user, body) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    let evaluation = evaluate(&snapshot);
    if evaluation.counts["upgradable"].as_i64().unwrap_or(0) > 0
        || evaluation.counts["missing"].as_i64().unwrap_or(0) > 0
    {
        let Some(queue) = state
            .jobs
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        else {
            return crate::http::store_wait::busy_response();
        };
        if let Err(response) = state
            .require_writable()
            .and_then(|_| state.require_write_admitted())
        {
            return response;
        }
        if let Err(error) = crate::jobs_api::seed_subscribe_search(
            &queue,
            &snapshot.subscribe.id.to_string(),
            Some(&snapshot.media.title),
        ) {
            tracing::error!(%error, subscribe_id=%snapshot.subscribe.id, "Subscribe upgrade queue failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job.seed_failed",
                &error.to_string(),
            );
        }
    }
    tracing::info!(subscribe_id=%snapshot.subscribe.id, units=evaluation.units.len(), "Subscribe upgrade evaluation completed");
    ok(json!({"summary":evaluation.summary, "target_label":snapshot.target_label,
        "filter_id":snapshot.subscribe.wash_cut_filter_id.or(Some(snapshot.subscribe.filter_id)).map(|id| id.to_string()),
        "counts":evaluation.counts, "units":evaluation.units})).into_response()
}

struct Evaluation {
    summary: String,
    counts: Value,
    units: Vec<Value>,
}

fn evaluate(snapshot: &Snapshot) -> Evaluation {
    let Snapshot {
        subscribe,
        media,
        facts,
        pending,
        history,
        target_score,
        target_label,
    } = snapshot;
    let mut units: Vec<Value> = Vec::new();
    let mut counts: std::collections::HashMap<&str, i64> = [
        ("upgradable", 0),
        ("at_cutoff", 0),
        ("in_flight", 0),
        ("not_comparable", 0),
        ("missing", 0),
    ]
    .into_iter()
    .collect();
    let requested = crate::open_coverage::units(&subscribe, &media, &facts, &pending, &history, []);
    let pending_index = super::wanted::PendingUnitIndex::new(&pending, &requested);
    for (season, episode) in requested {
        let fact = facts.get(season, episode);
        let in_flight = pending_index.get((season, episode), &pending).is_some();
        let state = unit_wash_state(subscribe, fact, in_flight, *target_score);
        *counts.get_mut(state).unwrap() += 1;
        units.push(json!({
            "season_number": season.unwrap_or(0) as i64,
            "episode_number": episode.unwrap_or(0) as i64,
            "state": state,
            "current_label": null,
            "target_label": if target_label.is_empty() { Value::Null } else { json!(target_label) },
        }));
    }
    let summary = upgrade_summary(&counts, &target_label, subscribe.wash_cut);
    let counts = json!({
        "upgradable": counts["upgradable"],
        "at_cutoff": counts["at_cutoff"],
        "in_flight": counts["in_flight"],
        "not_comparable": counts["not_comparable"],
        "missing": counts["missing"],
    });
    Evaluation {
        summary,
        counts,
        units,
    }
}

fn store_error(error: crate::store::StoreError) -> Response {
    tracing::error!(%error, "Subscribe upgrade capture failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
