//! Retry only dirty Subscribe Jobs configuration, preserving later manual Jobs edits.
use crate::jobs_writer::JobsMutationError;
use crate::{JobsReservation, management::ApiState};

pub(crate) fn reconcile_pending(state: &ApiState) -> Result<(), JobsMutationError> {
    reconcile_cleanup(state)?;
    let after = *state.subscribe_jobs_cursor.lock();
    let mut ids = state.store.pending_subscribe_job_sync(after, 64)?;
    if ids.is_empty() && after.is_some() {
        ids = state.store.pending_subscribe_job_sync(None, 64)?;
    }
    *state.subscribe_jobs_cursor.lock() = ids.last().copied();
    for id in ids {
        let guard = state.subscribe_guard(id);
        let Some(_guard) = guard.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT) else {
            tracing::warn!(subscribe_id = %id, "Subscribe Jobs recovery guard busy; deferred to next cursor round");
            continue;
        };
        if state.subscribe_is_deleting(id) {
            continue;
        }
        if let Err(error) = reconcile_one(state, id) {
            if matches!(
                error,
                JobsMutationError::Acquire(store::StoreError::WriteBusy)
            ) {
                return Err(error);
            }
            tracing::error!(subscribe_id = %id, %error, "Subscribe Jobs intent remains pending for retry");
        }
    }
    Ok(())
}

fn reconcile_one(state: &ApiState, id: domain::SubscribeId) -> Result<(), JobsMutationError> {
    let Some(intent) = state.store.subscribe_job_sync(id)? else {
        return Ok(());
    };
    let media = state
        .store
        .get_media(intent.subscribe.media_id)?
        .ok_or_else(|| {
            store::StoreError::Missing(format!("Media {}", intent.subscribe.media_id))
        })?;
    let reservation = state.jobs.reserve()?;
    // A successful foreground sync may have acknowledged this row while we
    // waited for admission. Never replay an already-clean intent over Jobs UI edits.
    let Some(current) = state.store.subscribe_job_sync(id)? else {
        return Ok(());
    };
    if current.generation != intent.generation || current.revision != intent.revision {
        return Ok(());
    }
    publish(&reservation, &intent, &media.title)?;
    if !state
        .store
        .acknowledge_subscribe_job_sync(id, &intent.generation, intent.revision)?
    {
        tracing::warn!(subscribe_id = %id, revision = intent.revision, "Subscribe Jobs intent changed before acknowledgment");
    }
    Ok(())
}

pub(crate) fn publish(
    reservation: &JobsReservation<'_>,
    intent: &store::SubscribeJobSync,
    title: &str,
) -> Result<(), JobsMutationError> {
    let queue = reservation.acquire()?;
    let subscribe = &intent.subscribe;
    queue.apply_subscribe_config(
        subscribe.id,
        &intent.generation,
        intent.epoch,
        intent.revision,
        |queue| {
            if queue
                .def_for_identity(jobs::JobIdentity::subscribe_search(subscribe.id))?
                .is_none()
            {
                crate::jobs_api::seed_subscribe_search_every(
                    &queue,
                    subscribe.id,
                    subscribe.search_interval_secs,
                    Some(title),
                )?;
            }
            crate::http::subscribe_schedule::sync_search_job(
                &queue,
                subscribe.id,
                subscribe.tracking_state == "paused",
            )?;
            crate::http::subscribe_schedule::set_search_interval(
                &queue,
                subscribe.id,
                subscribe.search_interval_secs,
            )?;
            if intent.create
                && queue
                    .def_for_identity(jobs::JobIdentity::catalog_refresh(subscribe.media_id))?
                    .is_none()
            {
                crate::jobs_api::seed_catalog_refresh(&queue, subscribe.media_id, Some(title))?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

fn reconcile_cleanup(state: &ApiState) -> Result<(), JobsMutationError> {
    let after = *state.subscribe_cleanup_cursor.lock();
    let mut intents = state.store.pending_subscribe_jobs_cleanup(after, 64)?;
    if intents.is_empty() && after.is_some() {
        intents = state.store.pending_subscribe_jobs_cleanup(None, 64)?;
    }
    *state.subscribe_cleanup_cursor.lock() = intents.last().map(|(id, _)| *id);
    for (id, generation) in intents {
        let guard = state.subscribe_guard(id);
        let Some(_guard) = guard.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT) else {
            continue;
        };
        if let Err(error) = cleanup_deleted(state, id, &generation) {
            tracing::warn!(subscribe_id=%id,%error,"Deleted Subscribe Jobs cleanup remains pending");
            if matches!(
                error,
                JobsMutationError::Acquire(store::StoreError::WriteBusy)
            ) {
                return Err(error);
            }
        }
    }
    Ok(())
}

// Caller holds the Subscribe decision guard; admission never crosses external IO.
pub(crate) fn cleanup_deleted(
    state: &ApiState,
    id: domain::SubscribeId,
    generation: &str,
) -> Result<(), JobsMutationError> {
    let reservation = state.jobs.reserve()?;
    if state.store.subscribe_jobs_cleanup(id)?.as_deref() != Some(generation) {
        return Ok(());
    }
    let epoch = state.store.subscribe_lifecycle_epoch(id, generation)?;
    reservation
        .acquire()?
        .close_subscribe_epoch(id, generation, epoch)?;
    // A new parent owns its definitions; acknowledge only the old tombstone.
    state
        .store
        .acknowledge_subscribe_jobs_cleanup(id, generation)?;
    Ok(())
}
