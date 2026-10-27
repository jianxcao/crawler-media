//! Watcher presence reconciliation: marks files missing instead of
//! automatically deleting file/Media/Playback facts.

use std::collections::HashMap;
use std::path::Path;

use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope,
};

use crate::library_presence::{PresenceError, PresencePublishError, publish_presence};
use crate::management::ApiState;

fn collect_event_observations(
    state: &ApiState,
    path: &Path,
    scope: &LibraryPathScope,
    candidates: Vec<LedgerPresenceObservation>,
) -> HashMap<String, Vec<LedgerPresenceObservation>> {
    let mut observations_by_lib: HashMap<String, Vec<LedgerPresenceObservation>> = HashMap::new();

    for mut observation in candidates {
        let file_path = observation.file.file.path.as_path();
        let matches_event = file_path == path || file_path.starts_with(path);
        if !matches_event {
            continue;
        }

        // A published file belongs to its recorded Library. The event path only
        // decides which rows moved, never who owns them.
        let Some(owner_lib) = scope
            .libraries()
            .find(|library| library.id == observation.file.file.library_id.to_string())
        else {
            continue;
        };

        let Ok(resolved) = LibraryPathScope::resolve_path(file_path) else {
            continue;
        };

        if !owner_lib.realtime_watch || !scope.accepts_work(&owner_lib.id) {
            continue;
        }

        let matching_root = owner_lib.root_paths.iter().find(|r| {
            resolved.starts_with(r)
                || LibraryPathScope::resolve_path(r).is_ok_and(|res_r| resolved.starts_with(&res_r))
        });
        let Some(root) = matching_root else {
            continue;
        };
        if state.library_files.readable_root(root).is_err() {
            tracing::warn!(library_id = %owner_lib.id, root = %root.display(), "Root unreadable during watcher event");
            continue;
        }

        observation.presence = match state.library_files.presence(file_path) {
            Ok(true) => FilePresence::Present,
            Ok(false) => FilePresence::Missing,
            Err(e) => {
                tracing::warn!(path = %file_path.display(), error = %e, "Watcher file presence failed");
                FilePresence::Unknown
            }
        };

        observations_by_lib
            .entry(owner_lib.id.clone())
            .or_default()
            .push(observation);
    }

    observations_by_lib
}

fn publish_event_observations(
    state: &ApiState,
    scope: &LibraryPathScope,
    observations_by_lib: HashMap<String, Vec<LedgerPresenceObservation>>,
) -> LedgerPresenceResult {
    let mut total_result = LedgerPresenceResult::default();
    let now = crate::job_loop::unix_now();

    for (lib_id, observations) in observations_by_lib {
        let lease = match state.acquire_library_work(&lib_id) {
            Ok(l) => l,
            Err(error) => {
                tracing::warn!(%error, library_id = %lib_id, "获取 Library 工作租约失败，跳过本次 watcher presence 发布");
                continue;
            }
        };

        let sub_res = publish_presence(&lease, || {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let current_snapshot = guard.library_ownership()?;
            if !current_snapshot.accepts_work(&lib_id) {
                return Err(PresencePublishError::LibraryDeleting);
            }
            if !scope.matches(&current_snapshot) {
                return Err(PresencePublishError::ScopeChanged);
            }
            guard
                .apply_ledger_presence(&observations, LedgerPresenceMode::Verify, now)
                .map_err(PresencePublishError::from)
        });

        match sub_res {
            Ok(res) => {
                state.probe.presence_committed(&res.applied);
                total_result.missing += res.missing;
                total_result.restored += res.restored;
                total_result.deleted += res.deleted;
                total_result.skipped += res.skipped;
            }
            Err(error) => {
                // 「library 正在删除 / scope 变化」是 watcher 的预期竞争：管理员冻结
                // Library 时 watcher 可能正在发布旧观察，这不是故障，不能打成 ERROR。
                if is_expected_presence_race(&error) {
                    tracing::debug!(library_id = %lib_id, error = %error, "watcher presence 发布被生命周期/scope 变化取消");
                } else {
                    tracing::error!(library_id = %lib_id, error = %error, "发布 watcher presence 失败");
                }
            }
        }
    }

    total_result
}

/// 预期竞争（Library 冻结/删除、scope 换代、租约取消）与真实故障的区分：
/// 按类型判定，不匹配文案 —— 改文案不会把真实故障变成"预期竞争"。
fn is_expected_presence_race(error: &PresenceError) -> bool {
    matches!(error, PresenceError::Deleting | PresenceError::ScopeChanged)
}

/// Observe a file event path and apply presence to matching active realtime libraries.
pub(crate) fn observe_event(
    state: &ApiState,
    path: &Path,
) -> Result<LedgerPresenceResult, PresenceError> {
    let snapshot = {
        let store_arc = state.store();
        let guard = store_arc.lock();
        guard.library_ownership()?
    };

    let scope = LibraryPathScope::resolve(snapshot)?;
    let mut total = LedgerPresenceResult::default();
    let mut cursor = None;
    loop {
        let candidates = {
            let store = state.store();
            let guard = store.lock();
            guard.watcher_presence_page(&path.to_string_lossy(), cursor.as_deref(), 100)?
        };
        let Some(last) = candidates.last() else {
            break;
        };
        cursor = Some(last.path_key.clone());
        let observations = collect_event_observations(state, path, &scope, candidates);
        let result = publish_event_observations(state, &scope, observations);
        total.missing += result.missing;
        total.restored += result.restored;
        total.deleted += result.deleted;
        total.skipped += result.skipped;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 Store 故障即使文案里带 `scope changed`，也不能被当成预期竞争：
    /// 旧实现按 `contains` 分类时，改一条错误文案就会把故障降级成 debug。
    #[test]
    fn store_failure_is_not_an_expected_race_even_when_text_looks_like_one() {
        assert!(!is_expected_presence_race(&PresenceError::Store(
            store::StoreError::Invalid("scope changed".into())
        )));
        assert!(!is_expected_presence_race(&PresenceError::Busy));
        assert!(!is_expected_presence_race(&PresenceError::Missing));
        assert!(is_expected_presence_race(&PresenceError::Deleting));
        assert!(is_expected_presence_race(&PresenceError::ScopeChanged));
    }
}
