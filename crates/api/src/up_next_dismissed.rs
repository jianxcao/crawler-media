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

/// Per-media diff token allowing best-effort compensation if a downstream
/// cross-DB write (`library.db`) fails after `app.db` was updated.
#[derive(Debug, Default)]
pub(crate) struct DismissedRollback {
    dismissed_at_secs: i64,
    prior_targets: Vec<(MediaId, Option<i64>)>,
    pruned: Vec<(MediaId, i64)>,
}

impl DismissedRollback {
    pub(crate) fn is_empty(&self) -> bool {
        self.prior_targets.is_empty() && self.pruned.is_empty()
    }
}

/// Record newly dismissed TV subscriptions and prune entries no longer backed by
/// an active TV subscription. Returns `(newly_dismissed_count, rollback_token)`.
pub(crate) fn record_dismissed(
    store: &Store,
    user_id: UserId,
    targets: &[MediaId],
    active_tv_subs: &HashSet<MediaId>,
    dismissed_at_secs: i64,
) -> Result<(usize, DismissedRollback), StoreError> {
    store.mutate_setting(&setting_key(user_id), |raw| {
        let existing = parse_dismissed(raw.as_deref(), user_id);
        let mut updated = existing.clone();
        let mut pruned = Vec::new();
        updated.retain(|media_id, ts| {
            let keep = active_tv_subs.contains(media_id);
            if !keep {
                pruned.push((*media_id, *ts));
            }
            keep
        });
        let mut newly_dismissed = 0usize;
        let mut seen_targets = HashSet::new();
        let mut prior_targets = Vec::new();
        for media_id in targets {
            if !active_tv_subs.contains(media_id) || !seen_targets.insert(*media_id) {
                continue;
            }
            let prior = existing.get(media_id).copied();
            if prior.is_none() {
                newly_dismissed += 1;
            }
            if prior != Some(dismissed_at_secs) {
                prior_targets.push((*media_id, prior));
            }
            updated.insert(*media_id, dismissed_at_secs);
        }
        if updated == existing {
            return Ok((raw, (newly_dismissed, DismissedRollback::default())));
        }
        let next = encode_dismissed(&updated)?;
        let rollback = DismissedRollback {
            dismissed_at_secs,
            prior_targets,
            pruned,
        };
        Ok((next, (newly_dismissed, rollback)))
    })
}

/// Best-effort per-media compensation when `library.db` fails after `record_dismissed`.
pub(crate) fn rollback_dismissed(store: &Store, user_id: UserId, rollback: &DismissedRollback) {
    if rollback.is_empty() {
        return;
    }
    if let Err(error) = store.mutate_setting(&setting_key(user_id), |raw| {
        let existing = parse_dismissed(raw.as_deref(), user_id);
        let mut updated = existing.clone();
        for (media_id, prior) in &rollback.prior_targets {
            if updated.get(media_id).copied() == Some(rollback.dismissed_at_secs) {
                match prior {
                    Some(prev_ts) => {
                        updated.insert(*media_id, *prev_ts);
                    }
                    None => {
                        updated.remove(media_id);
                    }
                }
            }
        }
        for (media_id, prev_ts) in &rollback.pruned {
            updated.entry(*media_id).or_insert(*prev_ts);
        }
        let next = if updated == existing {
            raw
        } else {
            encode_dismissed(&updated)?
        };
        Ok((next, ()))
    }) {
        tracing::warn!(%error, %user_id, "回滚继续观看隐藏列表失败");
    }
}

/// Unhide a media item when the user explicitly subscribes to it again.
/// Returns the removed `dismissed_at` timestamp if `media_id` was previously dismissed.
pub(crate) fn clear_dismissed_media(
    store: &Store,
    user_id: UserId,
    media_id: MediaId,
) -> Result<Option<i64>, StoreError> {
    store.mutate_setting(&setting_key(user_id), |raw| {
        let mut map = parse_dismissed(raw.as_deref(), user_id);
        if let Some(prev_ts) = map.remove(&media_id) {
            Ok((encode_dismissed(&map)?, Some(prev_ts)))
        } else {
            Ok((raw, None))
        }
    })
}

/// Restore a previously cleared dismissal when subscription creation fails.
pub(crate) fn restore_dismissed_media(
    store: &Store,
    user_id: UserId,
    media_id: MediaId,
    dismissed_at: i64,
) {
    if let Err(error) = store.mutate_setting(&setting_key(user_id), |raw| {
        let mut map = parse_dismissed(raw.as_deref(), user_id);
        if map.contains_key(&media_id) {
            return Ok((raw, ()));
        }
        map.insert(media_id, dismissed_at);
        Ok((encode_dismissed(&map)?, ()))
    }) {
        tracing::warn!(%error, %user_id, %media_id, "回滚订阅创建前的继续观看隐藏状态失败");
    }
}
