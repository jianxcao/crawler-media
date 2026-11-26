use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use domain::{MediaId, UserId};

use crate::store::{Store, StoreError};

fn setting_key(user_id: UserId) -> String {
    format!("user.{user_id}.up_next.dismissed")
}

fn parse_dismissed(raw: Option<&str>, user_id: UserId) -> HashMap<MediaId, i64> {
    let Some(raw) = raw else {
        return HashMap::new();
    };
    let parsed: HashMap<String, i64> = match serde_json::from_str(raw) {
        Ok(map) => map,
        Err(error) => {
            tracing::warn!(%error, %user_id, "解析继续观看隐藏列表失败，按空处理");
            return HashMap::new();
        }
    };
    parsed
        .into_iter()
        .filter_map(|(k, ts)| MediaId::from_str(&k).ok().map(|id| (id, ts)))
        .collect()
}

/// Load `{ media_id -> dismissed_at_unix_secs }` for one user from `app.db`.
pub(crate) fn load_dismissed(
    store: &Store,
    user_id: UserId,
) -> Result<HashMap<MediaId, i64>, StoreError> {
    let raw = store.get_setting(&setting_key(user_id))?;
    Ok(parse_dismissed(raw.as_deref(), user_id))
}

fn encode_dismissed(map: &HashMap<MediaId, i64>) -> Result<Option<String>, StoreError> {
    if map.is_empty() {
        return Ok(None);
    }
    let raw_map: HashMap<String, i64> = map.iter().map(|(id, ts)| (id.to_string(), *ts)).collect();
    serde_json::to_string(&raw_map)
        .map(Some)
        .map_err(|e| StoreError::Invalid(e.to_string()))
}

/// Record newly dismissed TV subscriptions and prune entries no longer backed by
/// an active TV subscription. Returns how many `targets` transitioned into the
/// dismissed set (used to count cleared items alongside `playback_units`).
pub(crate) fn record_dismissed(
    store: &Store,
    user_id: UserId,
    targets: &[MediaId],
    active_tv_subs: &HashSet<MediaId>,
    dismissed_at_secs: i64,
) -> Result<usize, StoreError> {
    store.mutate_setting(&setting_key(user_id), |raw| {
        let existing = parse_dismissed(raw.as_deref(), user_id);
        let mut updated = existing.clone();
        updated.retain(|media_id, _| active_tv_subs.contains(media_id));
        let mut newly_dismissed = 0usize;
        for media_id in targets {
            if !active_tv_subs.contains(media_id) {
                continue;
            }
            if !existing.contains_key(media_id) {
                newly_dismissed += 1;
            }
            updated.insert(*media_id, dismissed_at_secs);
        }
        let next = if updated == existing {
            raw
        } else {
            encode_dismissed(&updated)?
        };
        Ok((next, newly_dismissed))
    })
}

/// Unhide a media item when the user explicitly subscribes to it again.
pub(crate) fn clear_dismissed_media(
    store: &Store,
    user_id: UserId,
    media_id: MediaId,
) -> Result<(), StoreError> {
    store.mutate_setting(&setting_key(user_id), |raw| {
        let mut map = parse_dismissed(raw.as_deref(), user_id);
        if map.remove(&media_id).is_some() {
            Ok((encode_dismissed(&map)?, ()))
        } else {
            Ok((raw, ()))
        }
    })
}
