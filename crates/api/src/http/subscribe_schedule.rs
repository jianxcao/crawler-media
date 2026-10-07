//! Keep the SubscribeSearch JobDef in sync with Subscribe tracking_state / interval.

use jobs::{Queue, Schedule};

const MIN_INTERVAL_SECS: u32 = 300;
const MAX_INTERVAL_SECS: u32 = 86_400;
const DEFAULT_INTERVAL_SECS: u32 = 1_800;

/// Payload stored on the per-Subscribe search JobDef.
pub(crate) fn search_payload(subscribe_id: &str) -> String {
    format!(r#"{{"subscribe_id":"{subscribe_id}"}}"#)
}

pub(crate) fn clamp_interval(secs: u32) -> Result<u32, &'static str> {
    if (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&secs) {
        Ok(secs)
    } else {
        Err("search_interval_secs 必须在 300 到 86400 之间")
    }
}

pub(crate) fn default_interval() -> u32 {
    DEFAULT_INTERVAL_SECS
}

/// Pause stops scheduling and cancels queued children. Running searches check
/// the current Subscribe state before adding downloads.
pub(crate) fn sync_search_job(
    queue: &Queue,
    subscribe_id: &str,
    paused: bool,
) -> Result<(), jobs::JobError> {
    let payload = search_payload(subscribe_id);
    if queue.set_def_enabled(&payload, !paused)? == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows.into());
    }
    if paused {
        for def in queue.defs_for_payload(&payload)? {
            queue.cancel_def_children(def.id)?;
        }
    } else {
        // 只提前本订阅的 Search Job（按 payload 定位），不提前其他 def。
        queue.ensure_scheduled_for_payload(0, &payload)?;
    }
    Ok(())
}

pub(crate) fn set_search_interval(
    queue: &Queue,
    subscribe_id: &str,
    secs: u32,
) -> Result<(), jobs::JobError> {
    let payload = search_payload(subscribe_id);
    if queue.set_def_schedule(
        &payload,
        Schedule::Interval {
            secs: u64::from(secs),
        },
    )? == 0
    {
        return Err(rusqlite::Error::QueryReturnedNoRows.into());
    }
    Ok(())
}
