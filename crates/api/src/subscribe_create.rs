use domain::{Filter, Media, Subscribe};

use crate::management::ApiState;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CreateError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Admission(#[from] crate::jobs_writer::JobsMutationError),
    #[error(transparent)]
    Job(#[from] jobs::JobError),
    #[error("{0}")]
    Compensation(String),
    /// G-M1: another subscription of this media in the same library already
    /// covers some of these units.
    #[error("同一媒体库中已有覆盖重叠的订阅 {0}")]
    Overlap(domain::SubscribeId),
}

/// Persist a Subscribe and its scheduled Jobs as one logical operation.
/// The stores live in separate SQLite files, so failures need compensation.
pub(crate) fn create(
    state: &ApiState,
    incoming: Media,
    mut subscribe: Subscribe,
    filter: Option<&Filter>,
) -> Result<(Media, Subscribe), CreateError> {
    let reservation = state
        .jobs
        .reserve()
        .map_err(crate::jobs_writer::JobsMutationError::Acquire)?;
    let (media, previous) = {
        let store = state.store.as_ref();
        let (media, previous) = store.ensure_media_with_previous(incoming)?;
        subscribe.media_id = media.id;
        let inserted = (|| -> Result<(), StoreError> {
            if let Some(filter) = filter {
                store.insert_filter(filter)?;
            }
            store.insert_nonoverlapping_subscribe(&subscribe)
        })();
        if let Err(error) = inserted {
            if let Err(rollback) =
                rollback_store(&store, &media, previous.as_ref(), &subscribe, filter)
            {
                return Err(CreateError::Compensation(format!(
                    "{error}; compensation incomplete: {rollback}"
                )));
            }
            return Err(match error {
                StoreError::SubscribeOverlap { subscribe_id } => CreateError::Overlap(subscribe_id),
                error => error.into(),
            });
        }
        (media, previous)
    };

    let intent = state.store.subscribe_job_sync(subscribe.id)?;
    if let Err(error) = seed_jobs(&reservation, &media, &subscribe, intent.as_ref()) {
        tracing::error!(subscribe_id = %subscribe.id, error = %error, "创建订阅任务失败");
        if let Err(rollback) = rollback_store(
            &state.store.as_ref(),
            &media,
            previous.as_ref(),
            &subscribe,
            filter,
        ) {
            return Err(CreateError::Compensation(format!(
                "{error}; compensation incomplete; Jobs sync intent retained if Subscribe exists: {rollback}"
            )));
        }
        return Err(error.into());
    }
    if let Some(intent) = intent {
        state
            .store
            .acknowledge_subscribe_job_sync(subscribe.id, &intent.generation, intent.revision)
            .map_err(|error| {
                CreateError::Compensation(format!(
                    "Subscribe and Jobs created; acknowledgment pending: {error}"
                ))
            })?;
    }
    tracing::info!(subscribe_id = %subscribe.id, media = %media.title, "已创建新订阅");
    Ok((media, subscribe))
}

fn seed_jobs(
    reservation: &crate::JobsReservation<'_>,
    media: &Media,
    subscribe: &Subscribe,
    intent: Option<&store::SubscribeJobSync>,
) -> Result<(), crate::jobs_writer::JobsMutationError> {
    let intent = intent
        .ok_or_else(|| StoreError::Missing(format!("Subscribe Jobs intent {}", subscribe.id)))?;
    crate::subscribe_job_sync::publish(reservation, intent, &media.title)
}

fn rollback_store(
    store: &crate::Store,
    media: &Media,
    previous: Option<&Media>,
    subscribe: &Subscribe,
    filter: Option<&Filter>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Err(error) = store.delete_subscribe(subscribe.id) {
        tracing::error!(subscribe_id = %subscribe.id, error = %error, "回滚订阅失败");
        return Err(error.to_string());
    }
    if let Some(filter) = filter
        && let Err(error) = store.delete_filter(filter.id)
    {
        tracing::error!(filter_id = %filter.id, error = %error, "回滚规则组失败");
        failures.push(error.to_string());
    }
    match store.undo_ensured_media(media, previous) {
        Ok(true) => {}
        Ok(false) => {
            // `false` 有两种含义，都不能盲目回滚：
            // (a) 这行仍被 Subscribe/ledger 引用；(b) 身份已被并发请求改写（CAS 拒绝）。
            tracing::warn!(
                media_id = %media.id,
                "回滚 Media 被拒绝（仍被引用或身份已被并发改写），保留当前内容"
            );
        }
        Err(error) => {
            tracing::error!(media_id = %media.id, error = %error, "回滚 Media 失败");
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
