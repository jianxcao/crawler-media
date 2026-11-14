use jobs::{JobKind, NewDef, Queue, Schedule};
use serde_json::json;

pub fn seed_instance_defs(queue: &Queue) -> Result<(), jobs::JobError> {
    queue.ensure_def(NewDef {
        kind: JobKind::SubscribeRss,
        scope: jobs::JobScope::Global,
        name: "RSS".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 600 }),
        payload: "{}".into(),
        timeout_secs: Some(120),
        concurrency_key: Some("rss".into()),
    })?;
    queue.ensure_def(NewDef {
        kind: JobKind::Transfer,
        scope: jobs::JobScope::Global,
        name: "Transfer".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 30 }),
        payload: "{}".into(),
        timeout_secs: Some(120),
        concurrency_key: Some("transfer".into()),
    })?;
    queue.ensure_def(NewDef {
        kind: JobKind::WatchIntake,
        scope: jobs::JobScope::Global,
        name: "Watch intake".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 30 }),
        payload: "{}".into(),
        timeout_secs: Some(120),
        concurrency_key: Some("watch_intake".into()),
    })?;
    queue.ensure_def(NewDef {
        kind: JobKind::Scrape,
        scope: jobs::JobScope::Global,
        name: "Scrape".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 600 }),
        payload: "{}".into(),
        timeout_secs: Some(120),
        concurrency_key: Some("scrape".into()),
    })?;
    queue.ensure_def(NewDef {
        kind: JobKind::CheckIn,
        scope: jobs::JobScope::Global,
        name: "Check-in".into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 86400 }),
        payload: "{}".into(),
        timeout_secs: Some(120),
        concurrency_key: Some("check_in".into()),
    })?;
    queue.ensure_scheduled(0)?;
    Ok(())
}

pub fn seed_subscribe_search(
    queue: &Queue,
    subscribe_id: domain::SubscribeId,
    media_title: Option<&str>,
) -> Result<(), jobs::JobError> {
    let identity = jobs::JobIdentity::subscribe_search(subscribe_id);
    if queue.def_for_identity(identity)?.is_none() {
        return seed_subscribe_search_every(queue, subscribe_id, 1800, media_title);
    }
    // Manual search should not reset the configured interval.
    queue.ensure_scheduled_identity(0, identity)?;
    Ok(())
}

pub fn seed_subscribe_search_every(
    queue: &Queue,
    subscribe_id: domain::SubscribeId,
    interval_secs: u32,
    _media_title: Option<&str>,
) -> Result<(), jobs::JobError> {
    let secs = u64::from(interval_secs.max(300));
    let identity = jobs::JobIdentity::subscribe_search(subscribe_id);
    let payload = json!({ "subscribe_id": subscribe_id }).to_string();
    let name = format!("search {subscribe_id}");
    queue.ensure_def(NewDef {
        kind: JobKind::SubscribeSearch,
        scope: identity.scope,
        name,
        enabled: true,
        schedule: Some(Schedule::Interval { secs }),
        payload: payload.clone(),
        timeout_secs: Some(120),
        concurrency_key: Some(format!("subscribe:{subscribe_id}")),
    })?;
    queue.set_def_schedule(identity, Schedule::Interval { secs })?;
    // 只提前本订阅的 Search Job，不提前其他 def 的定时任务。
    queue.ensure_scheduled_identity(0, identity)?;
    Ok(())
}

pub fn seed_catalog_refresh(
    queue: &Queue,
    media_id: domain::MediaId,
    _media_title: Option<&str>,
) -> Result<(), jobs::JobError> {
    let identity = jobs::JobIdentity::catalog_refresh(media_id);
    let payload = json!({ "media_id": media_id }).to_string();
    let name = format!("catalog {media_id}");
    queue.ensure_def(NewDef {
        kind: JobKind::CatalogRefresh,
        scope: identity.scope,
        name,
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 86400 }),
        payload: payload.clone(),
        timeout_secs: Some(120),
        concurrency_key: Some(format!("catalog:{media_id}")),
    })?;
    queue.ensure_scheduled_identity(0, identity)?;
    Ok(())
}
