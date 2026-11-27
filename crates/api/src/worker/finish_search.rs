mod decision;

use subscribe::{RunInput, admit_and_add};

use super::{notify, notify_subscription_complete, record_wanted_round};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

/// Shared tail of every subscribe search: load facts, admit + add to the
/// downloader, record pending, notify.
pub(super) fn finish_search(
    state: &ApiState,
    initial_subscribe: &domain::Subscribe,
    mut torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<(), String> {
    let (subscribe, media, filter, _active) = {
        let guard = state.subscribe_guard(initial_subscribe.id);
        let _guard = guard.lock();
        if state.subscribe_is_deleting(initial_subscribe.id)
            || !subscribe_is_active(state, initial_subscribe.id)?
        {
            tracing::info!(subscribe_id = %initial_subscribe.id, "订阅在搜索期间已暂停或删除，跳过下载提交");
            return Ok(());
        }
        let Some(active) = decision::ActiveSearch::begin(state, initial_subscribe.id) else {
            tracing::debug!(subscribe_id = %initial_subscribe.id, "Subscribe round already active");
            return Ok(());
        };
        let store = state.store.as_ref();
        let Some(subscribe) = store
            .get_subscribe(initial_subscribe.id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let media = store
            .get_media(subscribe.media_id)
            .map_err(|e| e.to_string())?
            .ok_or("Media not found")?;
        let filter = store
            .get_filter(subscribe.filter_id)
            .map_err(|e| e.to_string())?
            .ok_or("Filter not found")?;
        (subscribe, media, filter, active)
    };

    let context = load_search_context(state, &subscribe, &media)?;

    tracing::info!(
        subscribe_id = %subscribe.id,
        media = %media.title,
        candidates = torrents.len(),
        "评估候选种子"
    );
    remove_pending_candidates(&mut torrents, &context.existing, subscribe.id);
    // H2: never auto-pick an enclosure whose probed quality already vetoed it.
    let rejected = state
        .store
        .as_ref()
        .load_rejected_enclosures(subscribe.id)
        .map_err(|e| e.to_string())?;
    if !rejected.is_empty() {
        torrents.retain(|torrent| !rejected.contains(&torrent.enclosure));
    }
    filter_active_defers(state, subscribe.id, &mut torrents);

    let policy_binding = {
        let guard = state.subscribe_guard(subscribe.id);
        let _guard = guard.lock();
        decision::ensure_current(state, &subscribe)?;
        ensure_library_active(state, &context.library_id)?;
        if !state
            .store
            .subscribe_snapshot_is_current(&context.snapshot, &Default::default())
            .map_err(|e| e.to_string())?
        {
            return Err("Library 配置或订阅事实在观察期间已变更，放弃本轮提交".into());
        }
        state
            .store
            .subscribe_policy_binding(subscribe.id)
            .map_err(|e| e.to_string())?
    };

    let add_errors = admit_candidates(
        state,
        &subscribe,
        &media,
        &filter,
        &context,
        &policy_binding,
        torrents,
        search_keywords,
    )?;
    notify_subscription_complete(state, &subscribe, &media)?;
    if !add_errors.is_empty() {
        return Err(format!(
            "{} torrent submission failure(s): {}",
            add_errors.len(),
            add_errors.join("; ")
        ));
    }
    Ok(())
}

struct SearchContext {
    facts: domain::SubscribeFacts,
    wash_filter: Option<domain::Filter>,
    existing: Vec<(i32, crate::store::PendingDownload)>,
    library_id: String,
    library_root: std::path::PathBuf,
    transfer_mode: Option<library::TransferMode>,
    scrape: bool,
    naming: String,
    snapshot: store::SubscribeLibrarySnapshot,
}

fn load_search_context(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> Result<SearchContext, String> {
    let obs = crate::subscribe_library_read::observe_subscribe_library_facts(
        state, subscribe, media.kind,
    )?;
    let facts = obs.facts;
    let library_id = obs.library_id;
    let snapshot = obs.snapshot;
    let wash_filter = {
        let store = state.store.as_ref();
        subscribe
            .wash_cut_filter_id
            .and_then(|id| store.get_filter(id).ok().flatten())
            .or_else(|| store.get_filter(subscribe.filter_id).ok().flatten())
    };
    let (library_root, transfer_mode, scrape) = crate::directory::transfer_plan_for_library(
        &state.store.as_ref(),
        media.kind,
        subscribe
            .library_id
            .as_ref()
            .map(|id| id.to_string())
            .as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let naming = state
        .store
        .as_ref()
        .naming_pattern(media.kind)
        .map_err(|error| error.to_string())?;
    let existing = state
        .store
        .as_ref()
        .load_pending(subscribe.id)
        .map_err(|error| error.to_string())?;
    Ok(SearchContext {
        facts,
        wash_filter,
        existing,
        library_id,
        library_root,
        transfer_mode,
        scrape,
        naming,
        snapshot,
    })
}

fn remove_pending_candidates(
    torrents: &mut Vec<domain::Torrent>,
    existing: &[(i32, crate::store::PendingDownload)],
    subscribe_id: domain::SubscribeId,
) {
    let pending: std::collections::HashSet<&str> = existing
        .iter()
        .map(|(_, item)| item.torrent.enclosure.as_str())
        .collect();
    let before = torrents.len();
    torrents.retain(|torrent| !pending.contains(torrent.enclosure.as_str()));
    if before != torrents.len() {
        tracing::debug!(subscribe_id = %subscribe_id, skipped = before - torrents.len(), "跳过已在下载中的 Torrent");
    }
}

fn subscribe_is_active(state: &ApiState, id: domain::SubscribeId) -> Result<bool, String> {
    Ok(state
        .store
        .as_ref()
        .get_subscribe(id)
        .map_err(|error| error.to_string())?
        .is_some_and(|subscribe| subscribe.tracking_state != "paused"))
}

fn ensure_library_active(state: &ApiState, library_id: &str) -> Result<(), String> {
    let active = !state.library_work().is_cancelled(library_id)
        && state
            .store
            .library_accepts_work(library_id)
            .map_err(|e| e.to_string())?;
    if !active {
        return Err("Library 配置或订阅事实在观察期间已变更，放弃本轮提交".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn admit_candidates(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    filter: &domain::Filter,
    context: &SearchContext,
    policy_binding: &Option<store::SubscribePolicyBinding>,
    torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<Vec<String>, String> {
    let routed =
        crate::delivery::RoutedDownloader::with_pending(state, subscribe, &context.existing);
    let facts = facts_with_pending(context.facts.clone(), &context.existing, subscribe, media);
    let overlap = overlap_policy(state);
    let absolute =
        crate::season_check::absolute_map(state, media, torrents.iter().map(|t| t.title.as_str()));
    let selective = crate::selective_files::enabled(state).then(|| facts.clone());
    let chooser = crate::pack_fallback::effective(state, subscribe);
    let submitter = crate::download_service::LibraryCancelSubmitter::new(
        state.clone(),
        context.library_id.clone(),
        subscribe.id,
        None,
    )
    .with_expected_subscribe(Some(subscribe.clone()));
    let policy_hooks = crate::plugins::hooks_runtime::ApiSubscribeHooks::new(state.clone());
    let added = admit_and_add(RunInput {
        subscribe: &chooser,
        media,
        filter,
        wash_filter: context.wash_filter.as_ref(),
        torrents,
        search_keywords,
        facts,
        downloader: &routed,
        library_root: &context.library_root,
        library_id: subscribe.library_id,
        root_id: None,
        config_revision: None,
        transfer_mode: context.transfer_mode,
        scrape: context.scrape,
        hooks: None,
        policy_hooks: Some(&policy_hooks),
        submitter: Some(&submitter),
        naming: Some(&context.naming),
        preserve_removed: false,
        overlap,
        absolute: absolute.as_ref(),
        renamed_destinations: Default::default(),
    })
    .map_err(|e| {
        tracing::error!(subscribe_id = %subscribe.id, media = %media.title, error = %e, "收录并添加种子失败");
        e.to_string()
    })?;
    for (enc, (dl_id, hash)) in &added.submitted_meta {
        routed.record_submitted_meta(enc, *dl_id, hash.clone());
    }
    {
        // An accepted external add remains tracked even if paused while it was
        // in flight. A late response must never recreate a deleted Subscribe.
        let guard = state.subscribe_guard(subscribe.id);
        let _guard = guard.lock();
        if state.subscribe_is_deleting(subscribe.id)
            || state
                .store
                .get_subscribe(subscribe.id)
                .map_err(|error| error.to_string())?
                .is_none()
        {
            return Err("Search Subscribe deleted during submission".into());
        }
        merge_added_pending(state, subscribe, &added, &routed)?;
        decision::ensure_current(state, subscribe)?;
        ensure_library_active(state, &context.library_id)?;
        record_select_defers(
            state,
            subscribe.id,
            policy_binding,
            added.select_deferred(),
            crate::job_loop::unix_now(),
        );
    }
    let deferred = selective
        .as_ref()
        .map(|before| {
            crate::selective_files::apply(
                &routed,
                subscribe,
                &added.chosen,
                before,
                absolute.as_ref(),
            )
        })
        .unwrap_or_default();
    // Catalog requests are observations, outside the Subscribe decision guard.
    let catalog_max = crate::air_gate::catalog_aired_max(state, subscribe, media);
    let cap = if subscribe.follow_future {
        None
    } else {
        catalog_max
    };
    {
        let guard = state.subscribe_guard(subscribe.id);
        let _guard = guard.lock();
        decision::ensure_current(state, subscribe)?;
        for (enclosure, missing) in &deferred {
            state
                .store
                .set_pending_selective(subscribe.id, enclosure, Some(missing))
                .map_err(|error| error.to_string())?;
        }
        ensure_library_active(state, &context.library_id)?;
        record_wanted_round(state, subscribe, &added, cap, catalog_max)?;
    }
    notify_search_additions(state, subscribe, media, &added);

    Ok(added.add_errors)
}

fn notify_search_additions<D: downloader::Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    added: &subscribe::Added<'_, D>,
) {
    if !added.chosen.is_empty() {
        tracing::info!(
            subscribe_id = %subscribe.id,
            media = %media.title,
            count = added.chosen.len(),
            "已收录新种子并提交下载"
        );
        notify(
            state,
            "订阅命中新资源",
            &format!(
                "「{}」新增 {} 个候选已提交下载",
                media.title,
                added.chosen.len()
            ),
        );
    } else {
        tracing::debug!(
            subscribe_id = %subscribe.id,
            media = %media.title,
            "本轮没有新收录的种子"
        );
    }
}

fn merge_added_pending<D: downloader::Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    added: &subscribe::Added<'_, D>,
    routed: &crate::delivery::RoutedDownloader<'_>,
) -> Result<(), String> {
    let mut pending: Vec<(i32, crate::store::PendingDownload)> = added
        .chosen
        .iter()
        .map(|scored| {
            let meta_dl_id = added
                .submitted_meta
                .get(&scored.torrent.enclosure)
                .and_then(|(id, _)| *id)
                .or_else(|| routed.used_id(&scored.torrent));
            (
                scored.score,
                crate::store::PendingDownload {
                    submitted_at: None,
                    torrent: scored.torrent.clone(),
                    release_override: None,
                    downloader_id: meta_dl_id,
                },
            )
        })
        .collect();
    for scored in &added.unknown {
        let meta_dl_id = added
            .submitted_meta
            .get(&scored.torrent.enclosure)
            .and_then(|(id, _)| *id);
        pending.push((
            scored.score,
            crate::store::PendingDownload {
                submitted_at: Some(0),
                torrent: scored.torrent.clone(),
                release_override: None,
                downloader_id: meta_dl_id,
            },
        ));
    }
    let store = state.store.as_ref();
    // D-M3: the same torrent reached through another enclosure (other site /
    // link) is one download: keep a single pending row per info hash.
    let known = store
        .load_pending_info_hashes(subscribe.id)
        .map_err(|error| error.to_string())?;
    let pending: Vec<_> = pending
        .into_iter()
        .filter(|(_, item)| {
            let Some(hash) = routed.submitted_hash(&item.torrent) else { return true };
            let duplicate = known
                .iter()
                .any(|(enclosure, known)| *enclosure != item.torrent.enclosure && known.eq_ignore_ascii_case(&hash));
            if duplicate {
                tracing::info!(torrent = %item.torrent.title, %hash, "同 info hash 的任务已在途，不重复登记 pending");
            }
            !duplicate
        })
        .collect();
    store
        .record_pending_submissions(subscribe.id, &pending)
        .map_err(|error| error.to_string())?;
    for (_, item) in &pending {
        if let Some(hash) = routed.submitted_hash(&item.torrent) {
            store
                .set_pending_info_hash(subscribe.id, &item.torrent.enclosure, &hash)
                .map_err(|error| error.to_string())?;
        }
    }
    // G-M2: a broken existing task is recorded as a rejected source so the
    // next round never picks it again and switches to another release.
    if !added.broken.is_empty() {
        let broken: Vec<(i32, crate::store::PendingDownload)> = added
            .broken
            .iter()
            .map(|scored| {
                (
                    scored.score,
                    crate::store::PendingDownload {
                        submitted_at: None,
                        torrent: scored.torrent.clone(),
                        release_override: None,
                        downloader_id: routed.used_id(&scored.torrent),
                    },
                )
            })
            .collect();
        let enclosures: Vec<String> = added
            .broken
            .iter()
            .map(|s| s.torrent.enclosure.clone())
            .collect();
        store
            .record_pending_submissions(subscribe.id, &broken)
            .map_err(|error| error.to_string())?;
        store
            .mark_pending_rejected(subscribe.id, &enclosures)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn record_select_defers(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    policy_binding: &Option<store::SubscribePolicyBinding>,
    select_deferred: &[(filter::ScoredTorrent, i64, String)],
    now_utc: i64,
) {
    if select_deferred.is_empty() {
        return;
    }
    let Some(binding) = policy_binding else {
        return;
    };
    let plugin_id = state
        .plugins
        .list_subscribe_traces(subscribe_id)
        .into_iter()
        .rev()
        .filter(|t| t.hook_id == "subscribe.select")
        .flat_map(|t| t.steps.into_iter().rev())
        .find(|s| {
            matches!(
                s.status,
                hooks::HookStepStatus::Deferred | hooks::HookStepStatus::Error
            )
        })
        .map(|s| s.plugin_id);
    for (scored, until_unix, reason) in select_deferred {
        match state
            .store
            .record_subscribe_defer(&store::RecordSubscribeDeferInput {
                defer_id: uuid::Uuid::new_v4().to_string(),
                subscribe_id,
                subscribe_generation: binding.jobs_sync_generation.clone(),
                policy_revision: binding.policy_revision,
                enclosure: scored.torrent.enclosure.clone(),
                info_hash: None,
                plugin_id: plugin_id.clone(),
                hook_id: "subscribe.select".to_string(),
                reason: hooks::sanitize_reason(reason),
                next_retry_at: *until_unix,
                now_utc,
            }) {
            Ok(_)
            | Err(store::StoreError::Plugin(
                store::plugins::PluginError::Denied | store::plugins::PluginError::StaleAuthority,
            )) => {}
            Err(err) => {
                tracing::warn!(%err, %subscribe_id, "record_subscribe_defer failed for subscribe.select");
            }
        }
    }
}

fn facts_with_pending(
    facts: domain::SubscribeFacts,
    existing: &[(i32, crate::store::PendingDownload)],
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> domain::SubscribeFacts {
    let releases = pending_releases(existing);
    subscribe::facts_with_pending(facts, &releases, subscribe, media)
}

pub(crate) fn pending_releases(
    existing: &[(i32, crate::store::PendingDownload)],
) -> Vec<domain::Release> {
    existing
        .iter()
        .map(|(_, pending)| {
            pending
                .release_override
                .clone()
                .unwrap_or_else(|| release::parse(&pending.torrent.title))
        })
        .collect()
}

/// M1: overlap policy from settings (`prefer_pack` default). Takes the store lock briefly.
pub(crate) fn overlap_policy(state: &ApiState) -> subscribe::OverlapPolicy {
    state
        .store
        .as_ref()
        .get_setting(crate::settings_keys::SUBSCRIBE_OVERLAP_POLICY)
        .ok()
        .flatten()
        .map(|value| subscribe::OverlapPolicy::parse(&value))
        .unwrap_or_default()
}

pub(crate) fn filter_active_defers(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    torrents: &mut Vec<domain::Torrent>,
) {
    let Ok(Some(binding)) = state.store.subscribe_policy_binding(subscribe_id) else {
        return;
    };
    let now = crate::job_loop::unix_now();
    torrents.retain(|t| {
        let Ok(Some(defer)) = state.store.get_subscribe_defer(subscribe_id, &t.enclosure) else {
            return true;
        };
        if defer.subscribe_generation != binding.jobs_sync_generation
            || defer.policy_revision != binding.policy_revision
        {
            return true;
        }
        match defer.state {
            store::SubscribeDeferState::Rejected => false,
            store::SubscribeDeferState::Deferred => defer.next_retry_at <= now,
            store::SubscribeDeferState::Invalidated | store::SubscribeDeferState::Resolved => true,
        }
    });
}
