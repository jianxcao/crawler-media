use std::str::FromStr;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Media, Subscribe, SubscribeId, UserId};

use crate::http::{err, ok};
use crate::management::ApiState;

use super::patch_fields::apply_patch_fields;
use super::types::PatchSubscriptionInput;

pub(crate) async fn patch_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
    crate::http::strict_json::StrictJson(body): crate::http::strict_json::StrictJson<
        PatchSubscriptionInput,
    >,
) -> Response {
    let blocking = state.clone();
    crate::http::subscriptions::guarded_blocking(&blocking, move || {
        patch_blocking(state, user_id, id, body)
    })
    .await
}

fn patch_blocking(
    state: ApiState,
    user_id: UserId,
    id: String,
    body: PatchSubscriptionInput,
) -> Response {
    let sid = match parse_subscribe_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let guard = state.subscribe_guard(sid);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(sid) {
        return err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除，无法修改",
        );
    }
    let auto_next_season = body.auto_next_season;
    let pack_fallback_after_days = body.pack_fallback_after_days;
    let (previous, subscribe, media) = match load_and_patch(&state, sid, user_id, body) {
        Ok(result) => result,
        Err(response) => return response,
    };
    if let Err(response) = persist_subscription(&state, &previous, &subscribe) {
        return response;
    }
    if let Some(days) = pack_fallback_after_days {
        crate::pack_fallback::set_after_days(&state, subscribe.id, (days > 0).then_some(days));
    }
    if let Some(auto) = auto_next_season {
        crate::completion::set_auto_next_season(&state, subscribe.id, auto);
    }
    drop(_guard);
    let snapshot = {
        let store = state.store.as_ref();
        let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
        super::snapshot::load_single_subscription_view(&store, &subscribe, &media, &facts)
    };
    let body = super::views::render_subscription_snapshot(&snapshot, state.file_stats.as_ref());
    ok(body).into_response()
}

fn parse_subscribe_id(id: &str) -> Result<SubscribeId, Response> {
    SubscribeId::from_str(id).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "订阅 id 无效",
        )
    })
}

fn load_and_patch(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
    body: PatchSubscriptionInput,
) -> Result<(Subscribe, Subscribe, Media), Response> {
    let store = state.store.as_ref();
    let Some(mut subscribe) = store.get_subscribe(id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    };
    super::ensure_subscribe_owner(&store, &subscribe, user_id)?;
    let previous = subscribe.clone();
    apply_patch_fields(&store, &mut subscribe, body)?;
    let Some(media) = store.get_media(subscribe.media_id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅关联的影视不存在",
        ));
    };
    Ok((previous, subscribe, media))
}

fn persist_subscription(
    state: &ApiState,
    previous: &Subscribe,
    subscribe: &Subscribe,
) -> Result<(), Response> {
    let reservation = state
        .jobs
        .reserve()
        .map_err(crate::http::jobs::jobs_acquire_response)?;
    if let Err(error) = state
        .store
        .as_ref()
        .update_nonoverlapping_subscribe(subscribe)
    {
        if let crate::store::StoreError::SubscribeOverlap { subscribe_id } = error {
            return Err(err(
                StatusCode::CONFLICT,
                "subscription.overlap",
                &format!("同一媒体库中已有覆盖重叠的订阅（{subscribe_id}），请合并或调整集数范围"),
            ));
        }
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ));
    }
    let intent = state
        .store
        .subscribe_job_sync(subscribe.id)
        .map_err(|error| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "subscription.job_sync_pending",
                &error.to_string(),
            )
        })?;
    let subscribe_id = subscribe.id.to_string();
    let sync = match intent.as_ref() {
        Some(intent) => crate::subscribe_job_sync::publish(&reservation, intent, ""),
        None => Ok(()),
    };
    if let Err(error) = sync {
        let compensation = rollback_subscription(state, &reservation, &subscribe_id, previous);
        tracing::error!(subscribe_id, error = %error, "Subscribe Jobs sync failed; compensation attempted and durable intent retained until convergence");
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subscription.job_sync_failed",
            &format!("{error}; {compensation}"),
        ));
    }
    if let Some(intent) = intent {
        state
            .store
            .acknowledge_subscribe_job_sync(subscribe.id, &intent.generation, intent.revision)
            .map_err(|error| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "subscription.job_sync_pending",
                    &error.to_string(),
                )
            })?;
    }
    Ok(())
}

fn rollback_subscription(
    state: &ApiState,
    reservation: &crate::JobsReservation<'_>,
    id: &str,
    previous: &Subscribe,
) -> String {
    if let Err(error) = state.store.as_ref().update_subscribe(previous) {
        tracing::error!(subscribe_id = id, %error, "Subscribe compensation failed; durable current-config intent will retry");
        // Jobs transaction rolled back, so leave durable current Subscribe state
        // to recovery rather than publishing an uncommitted previous config.
        return format!("Subscribe compensation failed; synchronization pending: {error}");
    }
    let intent = match state.store.subscribe_job_sync(previous.id) {
        Ok(Some(intent)) => intent,
        Ok(None) => return "Subscribe restored; no Jobs change pending".into(),
        Err(error) => {
            return format!("Subscribe restored; Jobs compensation intent read failed: {error}");
        }
    };
    if let Err(error) = crate::subscribe_job_sync::publish(reservation, &intent, "") {
        tracing::error!(subscribe_id = id, %error, "Jobs compensation failed; durable intent will retry");
        return format!("Jobs compensation failed; synchronization pending: {error}");
    }
    match state.store.acknowledge_subscribe_job_sync(
        previous.id,
        &intent.generation,
        intent.revision,
    ) {
        Ok(true) => "Subscribe and Jobs restored".into(),
        Ok(false) => "Subscribe compensation changed; synchronization pending".into(),
        Err(error) => {
            tracing::error!(subscribe_id = id, %error, "Jobs compensation committed; receipt preserves it until acknowledgment retry");
            format!("Jobs compensation committed; acknowledgment pending: {error}")
        }
    }
}
